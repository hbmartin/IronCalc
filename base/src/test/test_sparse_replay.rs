#![allow(clippy::unwrap_used, clippy::panic)]
use super::history::{Diff, DiffType, QueueDiffs};
use crate::{
    cf_types::CfRuleInput,
    expressions::types::Area,
    mock_time::{clock_reads, with_advancing_time},
    test::util::new_empty_model,
    types::{CalculationFailure, Cell, Dxf, DxfFont, Link, Workbook},
    Model, UserModel,
};
use std::collections::HashMap;

fn area(row: i32, column: i32, width: i32, height: i32) -> Area {
    Area {
        sheet: 0,
        row,
        column,
        width,
        height,
    }
}
fn cell_diff(sheet: u32, value: &str) -> Diff {
    Diff::SetCellValue {
        sheet,
        row: 1,
        column: 1,
        new_value: value.into(),
        old_value: Box::new(None),
    }
}
fn rule(formula: &str) -> CfRuleInput {
    CfRuleInput::Formula {
        formula: formula.into(),
        format: Dxf {
            font: Some(DxfFont {
                b: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
        stop_if_true: false,
    }
}
fn value(m: &UserModel, row: i32, column: i32) -> String {
    m.get_formatted_cell_value(0, row, column).unwrap()
}
type DisplayImage = (Vec<Option<Cell>>, HashMap<(u32, i32, i32), Link>);
fn image(m: &UserModel) -> DisplayImage {
    let saved: Workbook = bitcode::decode(&m.to_bytes()).unwrap();
    let ws = &saved.worksheets[0];
    let cells = (1..=8)
        .flat_map(|r| {
            (1..=8).map(move |c| {
                ws.cell(r, c)
                    .filter(|c| !matches!(&**c, Cell::EmptyCell { .. }))
                    .map(std::borrow::Cow::into_owned)
            })
        })
        .collect();
    (cells, m.model.links.clone())
}
fn assert_spill_owners(m: &UserModel) {
    let saved: Workbook = bitcode::decode(&m.to_bytes()).unwrap();
    for ws in &saved.worksheets {
        for (row, column, cell) in ws.sheet_data.cells() {
            if let Cell::SpillCell { a, .. } = &*cell {
                match ws.cell(a.0, a.1).as_deref() {
                    Some(Cell::ArrayFormula { r: (w, h), .. }) => {
                        assert!(row >= a.0 && row < a.0 + h && column >= a.1 && column < a.1 + w);
                    }
                    _ => panic!("orphaned spill at {row},{column}"),
                }
            }
        }
    }
}
fn link_model() -> UserModel<'static> {
    let mut raw = new_empty_model();
    raw._set("A1", "=HYPERLINK(\"https://example.com\",5)");
    raw._set("B1", "=A1*2");
    raw.evaluate();
    let mut m = UserModel::from_model(raw);
    m.add_conditional_formatting(0, "A1", rule("A1>3")).unwrap();
    m.add_conditional_formatting(0, "B1", rule("B1>8")).unwrap();
    m.flush_send_queue();
    m
}
fn assert_failed_cleanup(m: &UserModel, calculations: usize, reads: usize) {
    assert!(!m.model.workbook.worksheets[0]
        .cell(1, 1)
        .is_some_and(|c| c.has_formula()));
    assert!(m.model.links.is_empty());
    assert_eq!(value(m, 1, 2), "10");
    assert!(
        !m.model
            .get_extended_style_for_cell(0, 1, 1)
            .unwrap()
            .style
            .font
            .b
    );
    assert!(
        m.model
            .get_extended_style_for_cell(0, 1, 2)
            .unwrap()
            .style
            .font
            .b
    );
    assert_eq!(
        m.model.evaluation_report().failure,
        Some(CalculationFailure::NotEvaluated)
    );
    assert!(m.model.fork_evaluation_session().is_err());
    assert_eq!(m.model.evaluation.full_evaluations, calculations);
    assert_eq!(clock_reads(), reads);
}

#[test]
fn failed_undo_redo_and_external_replay_reconcile_unrecorded_mutations() {
    with_advancing_time(1_700_000_000_000, 1000, || {
        for paused in [false, true] {
            for mode in 0..3 {
                let mut m = link_model();
                if paused {
                    m.pause_evaluation();
                }
                let calculations = m.model.evaluation.full_evaluations;
                let reads = clock_reads();
                let diffs = vec![cell_diff(0, ""), cell_diff(999, "1")];
                let result = match mode {
                    0 => {
                        m.push_diff_list(vec![
                            cell_diff(999, "1"),
                            cell_diff(0, "=HYPERLINK(\"https://example.com\",5)"),
                        ]);
                        m.flush_send_queue();
                        m.undo()
                    }
                    1 => {
                        m.push_diff_list(diffs);
                        let entry = m.history.undo_stack.pop().unwrap();
                        m.history.redo_stack.push(entry);
                        m.flush_send_queue();
                        m.redo()
                    }
                    _ => m.apply_external_diffs(&bitcode::encode(&vec![QueueDiffs {
                        r#type: DiffType::Redo,
                        list: diffs,
                    }])),
                };
                assert!(result.is_err());
                assert_failed_cleanup(&m, calculations, reads);
                // Existing partial-failure semantics publish no outgoing batch.
                assert!(bitcode::decode::<Vec<QueueDiffs>>(&m.flush_send_queue())
                    .unwrap()
                    .is_empty());
            }
        }
    });
}

#[test]
fn raw_failed_batch_recovers_without_history_or_formula_calculation() {
    with_advancing_time(1_700_000_000_000, 1000, || {
        let mut m = link_model();
        let calculations = m.model.evaluation.full_evaluations;
        let reads = clock_reads();
        let result: Result<(), String> = m.model.with_display_batch(|raw| {
            raw.workbook.worksheets[0].cell_clear_contents(1, 1)?;
            Err("after write".into())
        });
        assert!(result.is_err());
        assert_failed_cleanup(&m, calculations, reads);
        m.set_user_input(0, 1, 1, "7").unwrap();
        assert_eq!(value(&m, 1, 2), "14");
        assert!(m.model.evaluation_report().converged);
    });
}

#[test]
fn rejected_mutation_free_replay_and_raw_batches_do_no_cleanup() {
    with_advancing_time(1_700_000_000_000, 1000, || {
        let mut m = link_model();
        m.model.evaluate_incremental(&[]);
        let bytes = m.to_bytes();
        let report = m.model.evaluation_report().clone();
        let index = m.model.dependency_index.as_ref().unwrap() as *const _;
        let links = m.model.links.clone();
        let visits = m.model.evaluation.display.link_cleanup_visits;
        let reads = clock_reads();
        for mode in 0..3 {
            let result: Result<(), String> = match mode {
                0 => {
                    m.push_diff_list(vec![cell_diff(999, "1")]);
                    m.undo()
                }
                1 => m.apply_external_diffs(&bitcode::encode(&vec![QueueDiffs {
                    r#type: DiffType::Redo,
                    list: vec![cell_diff(999, "1")],
                }])),
                _ => m
                    .model
                    .with_display_batch(|raw| raw.workbook.worksheet_mut(999).map(|_| ())),
            };
            assert!(result.is_err());
            assert_eq!(m.to_bytes(), bytes);
            assert_eq!(m.model.evaluation_report(), &report);
            assert_eq!(
                m.model.dependency_index.as_ref().unwrap() as *const _,
                index
            );
            assert_eq!(m.model.links, links);
            assert_eq!(m.model.evaluation.display.link_cleanup_visits, visits);
            assert_eq!(clock_reads(), reads);
        }
    });
}

#[test]
fn identity_reference_rewrites_omit_equal_supplements_and_keep_peer_results() {
    let mut raw = new_empty_model();
    raw._set("B100", "1");
    for row in 1..=50 {
        raw.set_user_input(0, row, 1, "=B100".into()).unwrap();
    }
    raw.evaluate();
    let mut m = UserModel::from_model(raw);
    m.insert_rows(0, 100, 1).unwrap();
    assert_eq!(m.history.undo_stack.last().unwrap().display.cell_count(), 0);
    assert_eq!(
        m.model.get_cell_formula(0, 1, 1).unwrap().as_deref(),
        Some("=B101")
    );
    let mut peer = UserModel::from_model(Model::from_bytes(&m.to_bytes(), "en").unwrap());
    peer.set_user_input(0, 101, 2, "20").unwrap();
    m.apply_external_diffs(&peer.flush_send_queue()).unwrap();
    m.pause_evaluation();
    for _ in 0..3 {
        m.undo().unwrap();
        for row in 1..=50 {
            assert_eq!(value(&m, row, 1), "20");
        }
        assert_eq!(value(&m, 100, 2), "20");
        m.redo().unwrap();
        for row in 1..=50 {
            assert_eq!(value(&m, row, 1), "20");
        }
        assert_eq!(value(&m, 101, 2), "20");
    }
}

#[test]
fn partial_spill_clear_and_cut_retain_equal_groups_for_paused_replay() {
    for paused in [false, true] {
        for cut in [false, true] {
            let mut raw = new_empty_model();
            raw._set("D1", "=SEQUENCE(2,2)");
            raw.evaluate();
            let mut m = UserModel::from_model(raw);
            let before = image(&m);
            if paused {
                m.pause_evaluation();
            }
            if cut {
                m.set_selected_cell(1, 5).unwrap();
                let cp = m.copy_to_clipboard().unwrap();
                m.set_selected_cell(4, 7).unwrap();
                m.paste_from_clipboard(0, cp.range, &cp.data, true).unwrap();
            } else {
                m.range_clear_contents(&area(1, 5, 1, 1)).unwrap();
            }
            let after = image(&m);
            assert!(m.history.undo_stack.last().unwrap().display.cell_count() >= 4);
            m.pause_evaluation();
            for _ in 0..3 {
                m.undo().unwrap();
                assert_eq!(image(&m), before);
                assert_spill_owners(&m);
                m.redo().unwrap();
                assert_eq!(image(&m), after);
                assert_spill_owners(&m);
            }
            m.resume_evaluation();
            m.evaluate();
            assert_spill_owners(&m);
        }
    }
}

#[test]
fn row_and_column_edits_through_spills_restore_all_children_and_removals() {
    for rows in [false, true] {
        for paused in [false, true] {
            for operation in 0..4 {
                for start in 1..=3 {
                    if operation == 3 && start == 1 {
                        continue;
                    }
                    let mut raw = new_empty_model();
                    raw._set("B2", "=SEQUENCE(2,2)");
                    raw.evaluate();
                    let mut m = UserModel::from_model(raw);
                    let before = image(&m);
                    if paused {
                        m.pause_evaluation();
                    }
                    match (rows, operation) {
                        (true, 0) => m.insert_rows(0, start, 1),
                        (false, 0) => m.insert_columns(0, start, 1),
                        (true, 1) => m.delete_rows(0, start, 1),
                        (false, 1) => m.delete_columns(0, start, 1),
                        (true, _) => {
                            m.move_rows_action(0, start, 1, if operation == 2 { 1 } else { -1 })
                        }
                        (false, _) => {
                            m.move_columns_action(0, start, 1, if operation == 2 { 1 } else { -1 })
                        }
                    }
                    .unwrap_or_else(|e| panic!("rows={rows}, op={operation}, start={start}: {e}"));
                    let after = image(&m);
                    m.pause_evaluation();
                    for _ in 0..2 {
                        m.undo().unwrap();
                        assert_eq!(
                            image(&m),
                            before,
                            "undo rows={rows}, paused={paused}, op={operation}, start={start}"
                        );
                        assert_spill_owners(&m);
                        m.redo().unwrap();
                        assert_eq!(
                            image(&m),
                            after,
                            "redo rows={rows}, paused={paused}, op={operation}, start={start}"
                        );
                        assert_spill_owners(&m);
                    }
                }
            }
        }
    }
}

#[test]
fn structural_redo_restores_absent_dynamic_link_tombstones() {
    for rows in [false, true] {
        let mut raw = new_empty_model();
        raw._set(
            "A1",
            if rows {
                "=IF(ROW()=1,HYPERLINK(\"https://example.com\",\"link\"),\"plain\")"
            } else {
                "=IF(COLUMN()=1,HYPERLINK(\"https://example.com\",\"link\"),\"plain\")"
            },
        );
        raw.evaluate();
        let mut m = UserModel::from_model(raw);
        if rows {
            m.insert_rows(0, 1, 1)
        } else {
            m.insert_columns(0, 1, 1)
        }
        .unwrap();
        let after = image(&m);
        assert!(m.model.links.is_empty());
        m.pause_evaluation();
        for _ in 0..3 {
            m.undo().unwrap();
            assert_eq!(m.model.links.len(), 1);
            m.redo().unwrap();
            assert_eq!(image(&m), after);
            assert!(m.model.links.is_empty());
        }
    }
}

#[test]
fn bounded_operation_sequences_round_trip_display_ownership() {
    for seed in 0..12_u32 {
        let mut state = seed + 1;
        let mut raw = new_empty_model();
        raw._set("B2", "=SEQUENCE(2,2)");
        raw._set("G7", "=7");
        raw.evaluate();
        let mut m = UserModel::from_model(raw);
        for step in 0..8 {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            let start = (state % 4 + 1) as i32;
            m.resume_evaluation();
            m.evaluate();
            let before = image(&m);
            if (state >> 8) & 1 != 0 {
                m.pause_evaluation();
            }
            m.with_user_display_edit(true, |m| match (state >> 16) % 5 {
                0 => m.insert_rows(0, start, 1),
                1 => m.delete_rows(0, start, 1),
                2 => m.insert_columns(0, start, 1),
                3 => m.move_columns_action(0, start, 1, 1),
                _ => m.range_clear_contents(&area(start, 3, 1, 1)),
            })
            .unwrap();
            let after = image(&m);
            m.pause_evaluation();
            m.undo().unwrap();
            assert_eq!(image(&m), before, "seed={seed}, step={step}, undo");
            assert_spill_owners(&m);
            m.redo().unwrap();
            assert_eq!(image(&m), after, "seed={seed}, step={step}, redo");
            assert_spill_owners(&m);
        }
    }
}

#[test]
fn group_capture_is_deduplicated_and_unrelated_equal_caches_stay_sparse() {
    let mut raw = new_empty_model();
    raw._set("B2", "=SEQUENCE(2,2)");
    for row in 20..=100 {
        raw.set_user_input(0, row, 10, "=7".into()).unwrap();
    }
    raw.evaluate();
    let mut m = UserModel::from_model(raw);
    m.with_user_display_edit(true, |m| {
        m.model.retain_spill_ownership(0, 2, 2);
        m.model.retain_spill_ownership(0, 2, 2);
        m.range_clear_contents(&area(2, 3, 1, 1))
    })
    .unwrap();
    assert_eq!(m.model.evaluation.display.ownership_groups, 2); // singleton + 2x2
    assert_eq!(m.history.undo_stack.last().unwrap().display.cell_count(), 4);
}

#[test]
fn identity_rewrites_preserve_peer_hyperlink_results_and_serialized_caches() {
    let mut raw = new_empty_model();
    raw._set("B100", "1");
    raw._set("A1", "=HYPERLINK(\"https://example.com/\"&B100,B100)");
    raw.evaluate();
    let mut m = UserModel::from_model(raw);
    m.insert_rows(0, 100, 1).unwrap();
    assert_eq!(m.history.undo_stack.last().unwrap().display.cell_count(), 0);
    let mut peer = UserModel::from_model(Model::from_bytes(&m.to_bytes(), "en").unwrap());
    peer.set_user_input(0, 101, 2, "20").unwrap();
    m.apply_external_diffs(&peer.flush_send_queue()).unwrap();
    let links = m.model.links.clone();
    m.pause_evaluation();
    for _ in 0..3 {
        m.undo().unwrap();
        assert_eq!(value(&m, 1, 1), "20");
        assert_eq!(m.model.links, links);
        let saved: Workbook = bitcode::decode(&m.to_bytes()).unwrap();
        assert!(matches!(
            saved.worksheets[0].cell(1, 1).as_deref(),
            Some(Cell::CellFormula {
                v: crate::types::FormulaValue::Number(20.0),
                ..
            })
        ));
        m.redo().unwrap();
        assert_eq!(value(&m, 1, 1), "20");
        assert_eq!(m.model.links, links);
    }
}

#[test]
fn failed_batches_invalidate_after_applied_writes_even_when_content_is_restored() {
    with_advancing_time(1_700_000_000_000, 1000, || {
        for operation in 0..4 {
            let mut raw = new_empty_model();
            raw._set("A1", "1");
            raw.evaluate();
            let saved = raw.workbook.worksheets[0].cell(1, 1).unwrap().into_owned();
            let bytes = raw.to_bytes();
            let reads = clock_reads();
            let result: Result<(), String> = raw.with_display_batch(|raw| {
                match operation {
                    0 => raw.workbook.worksheets[0].set_cell_with_number(1, 1, 2.0, 0)?,
                    1 => {
                        raw.workbook.worksheets[0].sheet_data.remove_cell(1, 1);
                    }
                    2 => raw.workbook.worksheets[0].sheet_data.remove_row(1),
                    _ => raw.workbook.worksheets[0].sheet_data.set_row(1, []),
                }
                raw.workbook.worksheets[0].update_cell(1, 1, saved)?;
                Err("after restored content".into())
            });
            assert!(result.is_err());
            assert_eq!(raw.to_bytes(), bytes);
            assert_eq!(
                raw.evaluation_report().failure,
                Some(CalculationFailure::NotEvaluated)
            );
            assert_eq!(clock_reads(), reads);
            assert!(raw.fork_evaluation_session().is_err());
        }
    });
}

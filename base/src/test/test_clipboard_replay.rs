#![allow(clippy::unwrap_used, clippy::panic)]

use super::history::{Diff, DiffType, QueueDiffs};
use crate::{
    cf_types::CfRuleInput,
    expressions::types::Area,
    mock_time::{clock_reads, with_advancing_time},
    test::util::new_empty_model,
    types::{ArrayKind, CalculationFailure, Cell, Dxf, DxfFont, Link, Workbook},
    Model, UserModel,
};
use std::sync::Arc;
fn area(row: i32, column: i32, width: i32, height: i32) -> Area {
    Area {
        sheet: 0,
        row,
        column,
        width,
        height,
    }
}
fn peer(m: &UserModel) -> UserModel<'static> {
    UserModel::from_model(Model::from_bytes(&m.to_bytes(), "en").unwrap())
}
fn val(m: &UserModel, row: i32, column: i32) -> String {
    m.get_formatted_cell_value(0, row, column).unwrap()
}
fn paste(m: &mut UserModel, source: (i32, i32, i32, i32), target: (i32, i32), cut: bool) {
    m.set_selected_cell(source.0, source.1).unwrap();
    m.set_selected_range(source.0, source.1, source.2, source.3)
        .unwrap();
    let cp = m.copy_to_clipboard().unwrap();
    m.set_selected_cell(target.0, target.1).unwrap();
    m.paste_from_clipboard(0, cp.range, &cp.data, cut).unwrap();
}
fn rule(formula: &str, bold: bool) -> CfRuleInput {
    CfRuleInput::Formula {
        formula: formula.into(),
        format: Dxf {
            font: Some(DxfFont {
                b: Some(bold),
                ..Default::default()
            }),
            ..Default::default()
        },
        stop_if_true: false,
    }
}
fn bold(m: &Model, row: i32, column: i32) -> bool {
    m.get_extended_style_for_cell(0, row, column)
        .unwrap()
        .style
        .font
        .b
}
fn is_cse(m: &UserModel, row: i32, column: i32, height: i32) {
    assert!(
        matches!(m.model.workbook.worksheets[0].cell(row,column).as_deref(),
        Some(Cell::ArrayFormula { kind: ArrayKind::Cse, r, .. }) if *r == (1,height))
    );
    for r in row + 1..row + height {
        assert!(
            matches!(m.model.workbook.worksheets[0].cell(r,column).as_deref(),
            Some(Cell::SpillCell { a, .. }) if *a == (row,column))
        );
    }
}

#[test]
fn nonempty_cse_destinations_restore_and_replay_complete_groups() {
    for paused in [false, true] {
        for height in [1, 2] {
            for destination in 0..3 {
                let mut m = UserModel::from_model(new_empty_model());
                m.set_user_array_formula(0, 1, 1, 1, height, "=123")
                    .unwrap();
                if destination == 2 {
                    m.set_user_array_formula(0, 1, 3, 1, height, "=456")
                        .unwrap();
                } else {
                    for r in 1..=height {
                        m.set_user_input(
                            0,
                            r,
                            3,
                            if destination == 0 {
                                if r == 1 {
                                    "x"
                                } else {
                                    "y"
                                }
                            } else {
                                "=HYPERLINK(\"https://example.com\",7)"
                            },
                        )
                        .unwrap();
                    }
                }
                m.set_cell_link(
                    0,
                    1,
                    3,
                    Link::External {
                        target: "https://manual.example".into(),
                        tooltip: None,
                    },
                    None,
                )
                .unwrap();
                let before: Vec<_> = (1..=height).map(|r| val(&m, r, 3)).collect();
                let links = m.get_links_list(0).unwrap();
                let mut p = peer(&m);
                p.evaluate();
                m.flush_send_queue();
                if paused {
                    m.pause_evaluation();
                    p.pause_evaluation();
                }
                paste(&mut m, (1, 1, height, 1), (1, 3), true);
                let payload = m.flush_send_queue();
                let queue: Vec<QueueDiffs> = bitcode::decode(&payload).unwrap();
                assert!(queue.iter().flat_map(|q| &q.list).any(|d| matches!(d,
                    Diff::RangeClearContents { column:3, height:h, .. } if *h == height)));
                p.apply_external_diffs(&payload).unwrap();
                is_cse(&m, 1, 3, height);
                is_cse(&p, 1, 3, height);
                let saved: Workbook = bitcode::decode(&m.to_bytes()).unwrap();
                assert!(matches!(
                    saved.worksheets[0].cell(1, 3).as_deref(),
                    Some(Cell::ArrayFormula {
                        kind: ArrayKind::Cse,
                        ..
                    })
                ));
                m.pause_evaluation();
                m.undo().unwrap();
                is_cse(&m, 1, 1, height);
                assert_eq!(
                    (1..=height).map(|r| val(&m, r, 3)).collect::<Vec<_>>(),
                    before
                );
                assert_eq!(m.get_links_list(0).unwrap(), links);
                if destination == 2 {
                    is_cse(&m, 1, 3, height);
                }
                let saved = Model::from_bytes(&m.to_bytes(), "en").unwrap();
                assert_eq!(
                    (1..=height)
                        .map(|r| saved.get_formatted_cell_value(0, r, 3).unwrap())
                        .collect::<Vec<_>>(),
                    before
                );
                p.apply_external_diffs(&m.flush_send_queue()).unwrap();
                p.evaluate();
                assert_eq!(
                    (1..=height).map(|r| val(&p, r, 3)).collect::<Vec<_>>(),
                    before
                );
                m.redo().unwrap();
                p.apply_external_diffs(&m.flush_send_queue()).unwrap();
                is_cse(&m, 1, 3, height);
                is_cse(&p, 1, 3, height);
                m.evaluate();
                p.evaluate();
                for r in 1..=height {
                    assert_eq!(val(&m, r, 3), "123");
                    assert_eq!(val(&p, r, 3), "123");
                }
            }
        }
    }
}

#[test]
fn overlapping_cse_targets_and_scalar_copies_preserve_destination_history() {
    let mut m = UserModel::from_model(new_empty_model());
    m.set_user_array_formula(0, 1, 1, 1, 2, "=123").unwrap();
    m.set_user_input(0, 3, 1, "x").unwrap();
    let mut p = peer(&m);
    m.flush_send_queue();
    m.pause_evaluation();
    p.pause_evaluation();
    paste(&mut m, (1, 1, 2, 1), (2, 1), true);
    p.apply_external_diffs(&m.flush_send_queue()).unwrap();
    is_cse(&m, 2, 1, 2);
    is_cse(&p, 2, 1, 2);
    m.undo().unwrap();
    p.apply_external_diffs(&m.flush_send_queue()).unwrap();
    is_cse(&m, 1, 1, 2);
    assert_eq!(val(&m, 3, 1), "x");
    m.redo().unwrap();
    p.apply_external_diffs(&m.flush_send_queue()).unwrap();
    m.evaluate();
    p.evaluate();
    assert_eq!(val(&m, 3, 1), val(&p, 3, 1));
    let mut m = UserModel::from_model(new_empty_model());
    m.set_user_input(0, 1, 1, "1").unwrap();
    m.set_user_input(0, 2, 1, "2").unwrap();
    m.set_user_array_formula(0, 1, 3, 1, 2, "=456").unwrap();
    let mut p = peer(&m);
    m.flush_send_queue();
    paste(&mut m, (1, 1, 2, 1), (1, 3), false);
    p.apply_external_diffs(&m.flush_send_queue()).unwrap();
    assert_eq!(val(&p, 2, 3), "2");
    m.pause_evaluation();
    m.undo().unwrap();
    is_cse(&m, 1, 3, 2);
    assert_eq!(val(&m, 2, 3), "456");
}

#[test]
fn rewritten_formulas_infer_formats_from_the_full_read_context() {
    for input in ["50%", "$12.50", "10/17/2026"] {
        for range in [false, true] {
            for cross_sheet in [false, true] {
                let mut raw = new_empty_model();
                if cross_sheet {
                    raw.new_sheet();
                }
                let sheet = u32::from(cross_sheet);
                raw.set_user_input(0, 1, 1, "1".into()).unwrap();
                raw.set_user_input(sheet, 1, 3, input.into()).unwrap();
                let prefix = if cross_sheet { "Sheet2!" } else { "" };
                let formula = if range {
                    format!("=A1+SUM({prefix}C1:C3)")
                } else {
                    format!("=A1+{prefix}C1")
                };
                raw.set_user_input(0, 1, 2, formula).unwrap();
                let expected = raw.get_style_for_cell(sheet, 1, 3).unwrap().num_fmt;
                let mut style = raw.get_style_for_cell(0, 1, 2).unwrap();
                style.num_fmt = "General".into();
                raw.set_cell_style(0, 1, 2, &style).unwrap();
                raw.evaluate();
                let mut m = UserModel::from_model(raw);
                let mut p = peer(&m);
                m.flush_send_queue();
                m.pause_evaluation();
                p.pause_evaluation();
                paste(&mut m, (1, 1, 1, 1), (2, 1), true);
                let bytes = m.flush_send_queue();
                let queue: Vec<QueueDiffs> = bitcode::decode(&bytes).unwrap();
                assert!(queue.iter().flat_map(|q| &q.list).any(|d| matches!(
                    d,
                    Diff::SetCellStyle {
                        sheet: 0,
                        row: 1,
                        column: 2,
                        ..
                    }
                )));
                p.apply_external_diffs(&bytes).unwrap();
                assert_eq!(
                    m.model.get_style_for_cell(0, 1, 2).unwrap().num_fmt,
                    expected
                );
                assert_eq!(
                    p.model.get_style_for_cell(0, 1, 2).unwrap().num_fmt,
                    expected
                );
                m.evaluate();
                p.evaluate();
                assert_eq!(val(&m, 1, 2), val(&p, 1, 2));
                m.undo().unwrap();
                assert_eq!(
                    m.model.get_style_for_cell(0, 1, 2).unwrap().num_fmt,
                    "General"
                );
                m.redo().unwrap();
                assert_eq!(
                    m.model.get_style_for_cell(0, 1, 2).unwrap().num_fmt,
                    expected
                );
            }
        }
    }
}

#[test]
fn raw_formatting_mutations_refresh_without_invalidating_calculation() {
    with_advancing_time(1_700_000_000_000, 1000, || {
        let mut raw = new_empty_model();
        raw._set("A1", "5");
        raw._set("D1", "=RAND()");
        raw.evaluate();
        raw.evaluate_incremental(&[]);
        let report = raw.evaluation_report().clone();
        let value = raw._get_text("D1");
        let index = Arc::clone(raw.dependency_index.as_ref().unwrap());
        let reads = clock_reads();
        let evals = raw.evaluation.full_evaluations;
        raw.add_conditional_formatting(0, "B1", rule("$A$1>3", true))
            .unwrap();
        assert!(bold(&raw, 1, 2));
        raw.update_conditional_formatting(0, 0, "B1", rule("FALSE", true))
            .unwrap();
        assert!(!bold(&raw, 1, 2));
        raw.update_conditional_formatting(0, 0, "B1", rule("TRUE", true))
            .unwrap();
        raw.add_conditional_formatting(0, "B1", rule("TRUE", false))
            .unwrap();
        let previous = bold(&raw, 1, 2);
        raw.raise_conditional_formatting_priority(0, 0).unwrap();
        assert_ne!(bold(&raw, 1, 2), previous);
        let cf = raw.evaluation.cf_evaluations;
        raw.raise_conditional_formatting_priority(0, 0).unwrap();
        assert_eq!(raw.evaluation.cf_evaluations, cf);
        raw.lower_conditional_formatting_priority(0, 0).unwrap();
        assert_eq!(bold(&raw, 1, 2), previous);
        raw.delete_conditional_formatting(0, 1).unwrap();
        assert!(bold(&raw, 1, 2));
        raw.delete_conditional_formatting(0, 0).unwrap();
        assert!(!bold(&raw, 1, 2));
        assert_eq!(raw.evaluation_report(), &report);
        assert_eq!(raw._get_text("D1"), value);
        assert!(Arc::ptr_eq(&index, raw.dependency_index.as_ref().unwrap()));
        assert_eq!(raw.evaluation.full_evaluations, evals);
        assert_eq!(clock_reads(), reads);
        let mut m = UserModel::from_model(raw);
        let cf = m.model.evaluation.cf_evaluations;
        m.with_user_display_edit(false, |m| {
            m.add_conditional_formatting(0, "B1", rule("TRUE", true))?;
            m.update_conditional_formatting(0, 0, "C1", rule("TRUE", true))?;
            Ok(())
        })
        .unwrap();
        assert_eq!(m.model.evaluation.cf_evaluations, cf + 1);
        assert!(!bold(&m.model, 1, 2));
        assert!(bold(&m.model, 1, 3));
        let cf = m.model.evaluation.cf_evaluations;
        let history = m.history.undo_stack.len();
        m.raise_conditional_formatting_priority(0, 0).unwrap();
        m.lower_conditional_formatting_priority(0, 0).unwrap();
        assert_eq!(m.model.evaluation.cf_evaluations, cf);
        assert_eq!(m.history.undo_stack.len(), history);
        assert_eq!(clock_reads(), reads);
    });
}

#[test]
fn failed_live_batches_reconcile_links_and_formatting_without_calculation() {
    with_advancing_time(1_700_000_000_000, 1000, || {
        for paused in [false, true] {
            let mut m = UserModel::from_model(new_empty_model());
            m.set_user_input(0, 1, 1, "=HYPERLINK(\"https://example.com\",5)")
                .unwrap();
            m.add_conditional_formatting(0, "B1", rule("$A$1>3", true))
                .unwrap();
            assert!(bold(&m.model, 1, 2));
            assert_eq!(m.get_links_list(0).unwrap().len(), 1);
            m.flush_send_queue();
            if paused {
                m.pause_evaluation();
            }
            let evals = m.model.evaluation.full_evaluations;
            let reads = clock_reads();
            let cf = m.model.evaluation.cf_evaluations;
            let bytes = bitcode::encode(&vec![QueueDiffs {
                r#type: DiffType::Redo,
                list: vec![
                    Diff::RangeClearContents {
                        sheet: 0,
                        row: 1,
                        column: 1,
                        width: 1,
                        height: 1,
                        old_value: vec![vec![None]],
                    },
                    Diff::SetCellValue {
                        sheet: 99,
                        row: 1,
                        column: 1,
                        new_value: "1".into(),
                        old_value: Box::new(None),
                    },
                ],
            }]);
            assert!(m.apply_external_diffs(&bytes).is_err());
            assert_eq!(val(&m, 1, 1), "");
            assert!(m.get_links_list(0).unwrap().is_empty());
            assert!(!bold(&m.model, 1, 2));
            assert_eq!(m.model.evaluation.full_evaluations, evals);
            assert_eq!(m.model.evaluation.cf_evaluations, cf + 1);
            assert_eq!(clock_reads(), reads);
            assert_eq!(
                m.model.evaluation_report().failure,
                Some(CalculationFailure::NotEvaluated)
            );
            assert_eq!(m.model.evaluation.display.edit_depth, 0);
            m.set_user_input(0, 1, 1, "9").unwrap();
            m.evaluate();
            assert_eq!(val(&m, 1, 1), "9");
        }
    });
}

#[test]
fn rejected_context_changes_leave_pending_formatting_recovery_untouched() {
    with_advancing_time(1_700_000_000_000, 1000, || {
        let mut m = UserModel::from_model(new_empty_model());
        m.set_user_input(0, 1, 1, "=NOW()").unwrap();
        m.add_conditional_formatting(0, "B1", rule("TRUE", true))
            .unwrap();
        m.pause_evaluation();
        m.model.evaluation.display.formatting_recovery = true;
        let bytes = m.to_bytes();
        let cf = m.model.evaluation.cf_evaluations;
        let report = m.model.evaluation_report().clone();
        let reads = clock_reads();
        assert!(m.set_locale("fr").is_err());
        assert_eq!(m.to_bytes(), bytes);
        assert_eq!(m.model.evaluation.cf_evaluations, cf);
        assert_eq!(m.model.evaluation_report(), &report);
        assert_eq!(clock_reads(), reads);
        assert!(m.model.evaluation.display.formatting_recovery);
        m.set_columns_width(0, 1, 1, 80.0).unwrap();
        assert_eq!(m.model.evaluation.cf_evaluations, cf + 1);
        assert!(!m.model.evaluation.display.formatting_recovery);
    });
}

#[test]
fn pending_scalar_clears_join_empty_and_partial_incremental_edit_lists() {
    for all in [false, true] {
        for headless in [false, true] {
            let mut m = new_empty_model();
            for (cell, value) in [
                ("A1", "1"),
                ("A2", "2"),
                ("B1", "=A1*2"),
                ("B2", "=A2*2"),
                ("C1", "=SUM(A1:A10)"),
                ("D1", "=B1+B2+C1"),
                ("E1", "=3"),
                ("F1", "=E1+1"),
            ] {
                m._set(cell, value);
            }
            m.evaluate();
            m.evaluate_incremental(&[]);
            for a in [area(1, 1, 1, 1), area(2, 1, 1, 9), area(1, 5, 1, 1)] {
                if all {
                    m.range_clear_all(&a).unwrap();
                } else {
                    m.range_clear_contents(&a).unwrap();
                }
            }
            assert_eq!(
                m.evaluation.pending_scalar_clears.as_ref().unwrap().len(),
                3
            );
            assert!(m.fork_evaluation_session().is_err());
            assert!(!m.evaluation_report().converged);
            let count = m.evaluation.full_evaluations;
            let result = if headless {
                m.evaluate_incremental_headless(&[])
            } else {
                m.evaluate_incremental(&[(0, 2, 1)])
            };
            assert!(result.is_some());
            assert_eq!(m.evaluation.full_evaluations, count);
            for cell in ["B1", "B2", "C1", "D1"] {
                assert_eq!(m._get_text(cell), "0");
            }
            assert_eq!(m._get_text("F1"), "1");
            assert!(m.evaluation.pending_scalar_clears.is_none());
            assert!(m.evaluation_report().converged);
            assert!(m.evaluate_incremental(&[]).unwrap().is_empty());
        }
    }
}

#[test]
fn conservative_invalidations_retire_pending_clear_baselines() {
    for kind in 0..6 {
        let mut m = new_empty_model();
        m._set("A1", "1");
        m._set("B1", "=A1*2");
        m.evaluate();
        m.evaluate_incremental(&[]);
        m.range_clear_contents(&area(1, 1, 1, 1)).unwrap();
        match kind {
            0 => m.set_max_lambda_depth(255),
            1 => m.set_max_evaluation_stack(1024 * 1024),
            2 => m.set_require_two_iterative_passes(false),
            3 => m.insert_rows(0, 10, 1).unwrap(),
            4 => m = Model::from_bytes(&m.to_bytes(), "en").unwrap(),
            _ => m.invalidate_calculation(),
        }
        assert!(m.evaluation.pending_scalar_clears.is_none());
        assert!(m.evaluate_incremental(&[]).is_none());
        assert_eq!(m._get_text("B1"), "0");
    }
    let mut m = new_empty_model();
    m._set("A1", "1");
    m._set("B1", "=A1*2");
    m.evaluate();
    m.range_clear_contents(&area(1, 1, 1, 1)).unwrap();
    m.workbook.settings.iterative_calculation.enabled = true;
    m.workbook.settings.iterative_calculation.maximum_iterations = 0;
    assert!(m.evaluate_incremental(&[]).is_none());
    assert_eq!(
        m.evaluation_report().failure,
        Some(CalculationFailure::InvalidSettings)
    );
    assert!(m.evaluation.pending_scalar_clears.is_none());
}

#[test]
fn unexpected_prepared_writes_are_rejected_atomically() {
    fn cell(m: &mut Model<'_>) {
        m.set_user_input(0, 50, 50, "=9".into()).unwrap();
    }
    fn dynamic(m: &mut Model<'_>) {
        m.insert_dynamic_link(
            (0, 50, 50),
            Link::External {
                target: "https://unexpected.example".into(),
                tooltip: None,
            },
        );
    }
    fn manual(m: &mut Model<'_>) {
        m.workbook.worksheets[0].links.insert(
            (50, 50),
            Link::External {
                target: "https://unexpected.example".into(),
                tooltip: None,
            },
        );
    }
    with_advancing_time(1_700_000_000_000, 1000, || {
        for hook in [cell as fn(&mut Model<'_>), dynamic, manual] {
            let mut m = UserModel::from_model(new_empty_model());
            m.set_user_input(0, 1, 1, "1").unwrap();
            m.set_selected_cell(1, 1).unwrap();
            let cp = m.copy_to_clipboard().unwrap();
            m.set_selected_cell(3, 3).unwrap();
            m.flush_send_queue();
            let bytes = m.to_bytes();
            let report = m.model.evaluation_report().clone();
            let view = m.get_selected_view();
            let reads = clock_reads();
            let history = m.history.undo_stack.len();
            let formats = m.model.evaluation.cf_evaluations;
            m.model.evaluation.display.paste_hook = Some(hook);
            assert!(m
                .paste_from_clipboard(0, cp.range, &cp.data, true)
                .unwrap_err()
                .contains("footprint"));
            assert_eq!(m.to_bytes(), bytes);
            assert_eq!(m.model.evaluation_report(), &report);
            assert_eq!(m.get_selected_view(), view);
            assert_eq!(clock_reads(), reads);
            assert_eq!(m.history.undo_stack.len(), history);
            assert_eq!(m.model.evaluation.cf_evaluations, formats);
            assert!(bitcode::decode::<Vec<QueueDiffs>>(&m.flush_send_queue())
                .unwrap()
                .is_empty());
            m.model.evaluation.display.paste_hook = None;
            m.paste_from_clipboard(0, cp.range, &cp.data, true).unwrap();
            assert_eq!(val(&m, 3, 3), "1");
        }
    });
}

#[test]
fn cse_discovery_ignores_empty_rectangle_positions_and_unrelated_cells() {
    let mut raw = new_empty_model();
    raw.set_user_array_formula(0, 1, 1, 1, 2, "=123").unwrap();
    raw.set_user_array_formula(0, 10000, 100, 1, 1, "=456")
        .unwrap();
    for r in 1..=1000 {
        raw.set_user_input(0, r, 200, "=1".into()).unwrap();
    }
    let m = UserModel::from_model(raw);
    let (groups, visits) = m.clipboard_cse_sources(0, (1, 1, 10000, 100)).unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(visits, 3);
}

#[test]
fn prepared_replay_excludes_unrelated_caches_and_keeps_equal_required_values() {
    let mut raw = new_empty_model();
    raw._set("A1", "=7");
    raw._set("C1", "=7");
    raw._set("D1", "=A1*2");
    for c in 10..=200 {
        raw.set_user_input(0, 1, c, "=9".into()).unwrap();
    }
    raw.evaluate();
    let mut m = UserModel::from_model(raw);
    paste(&mut m, (1, 1, 1, 1), (1, 3), true);
    let entry = m.history.undo_stack.last().unwrap();
    assert!(entry.display.cell_count() <= 3);
    let display = Arc::clone(&entry.display);
    m.pause_evaluation();
    m.undo().unwrap();
    assert_eq!(val(&m, 1, 1), "7");
    assert_eq!(val(&m, 1, 3), "7");
    assert_eq!(val(&m, 1, 4), "14");
    m.redo().unwrap();
    assert_eq!(val(&m, 1, 3), "7");
    assert_eq!(val(&m, 1, 4), "14");
    assert!(Arc::ptr_eq(
        &display,
        &m.history.undo_stack.last().unwrap().display
    ));
    let saved = Model::from_bytes(&m.to_bytes(), "en").unwrap();
    assert_eq!(saved.get_formatted_cell_value(0, 1, 4).unwrap(), "14");
}

#[test]
fn equal_rewritten_arrays_and_hyperlinks_survive_paused_redo() {
    let mut raw = new_empty_model();
    raw._set("A1", "7");
    raw.set_user_array_formula(0, 1, 4, 1, 2, "=A1*2").unwrap();
    raw._set("H1", "=HYPERLINK(\"https://example.com\",A1*2)");
    raw.evaluate();
    let mut m = UserModel::from_model(raw);
    let links = m.get_links_list(0).unwrap();
    paste(&mut m, (1, 1, 1, 1), (1, 3), true);
    m.pause_evaluation();
    for _ in 0..2 {
        m.undo().unwrap();
        assert_eq!(val(&m, 1, 4), "14");
        assert_eq!(val(&m, 2, 4), "14");
        assert_eq!(m.get_links_list(0).unwrap(), links);
        m.redo().unwrap();
        assert_eq!(val(&m, 1, 4), "14");
        assert_eq!(val(&m, 2, 4), "14");
        assert_eq!(val(&m, 1, 8), "14");
        assert_eq!(m.get_links_list(0).unwrap(), links);
    }
}

#![allow(clippy::unwrap_used, clippy::panic)]
use super::history::{Diff, DiffType, QueueDiffs};
use crate::{
    cf_types::CfRuleInput,
    expressions::types::Area,
    mock_time::{clock_reads, with_advancing_time},
    test::util::new_empty_model,
    types::{CalculationFailure, Cell, Dxf, DxfFont, Workbook},
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
fn value(m: &UserModel, row: i32, column: i32) -> String {
    m.get_formatted_cell_value(0, row, column).unwrap()
}
fn rule() -> CfRuleInput {
    CfRuleInput::Formula {
        formula: "B1>8".into(),
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
fn saved(m: &UserModel, row: i32, column: i32) -> Cell {
    let w: Workbook = bitcode::decode(&m.to_bytes()).unwrap();
    w.worksheets[0].cell(row, column).unwrap().into_owned()
}
#[test]
fn ordinary_equal_formula_recreation_survives_paused_replay() {
    for (old, new) in [
        ("=A1+1", "=A1*2"),
        ("=A1+1", "=A1+1"),
        ("=1+1", "=2"),
        (
            "=HYPERLINK(\"https://example.com\",A1+1)",
            "=HYPERLINK(\"https://example.com\",A1*2)",
        ),
    ] {
        let mut m = UserModel::from_model(new_empty_model());
        m.set_user_input(0, 1, 1, "1").unwrap();
        m.set_user_input(0, 1, 2, old).unwrap();
        m.set_user_input(0, 1, 2, new).unwrap();
        let cell = saved(&m, 1, 2);
        let links = m.get_links_list(0).unwrap();
        let supplement = Arc::clone(&m.history.undo_stack.last().unwrap().display);
        assert_eq!(supplement.cell_count(), 1);
        m.pause_evaluation();
        for _ in 0..2 {
            m.undo().unwrap();
            assert_eq!(value(&m, 1, 2), "2");
            m.redo().unwrap();
            assert_eq!(value(&m, 1, 2), "2");
            assert_eq!(saved(&m, 1, 2), cell);
            assert_eq!(m.get_links_list(0).unwrap(), links);
            assert!(m.model.fork_evaluation_session().is_err());
        }
        assert!(Arc::ptr_eq(
            &supplement,
            &m.history.undo_stack.last().unwrap().display
        ));
    }
}
#[test]
fn equal_array_recreation_restores_complete_spills() {
    for cse in [false, true] {
        let mut m = UserModel::from_model(new_empty_model());
        if cse {
            m.set_user_array_formula(0, 1, 2, 1, 2, "=ROW(A1:A2)")
                .unwrap();
            let old_values = vec![vec![Some(saved(&m, 1, 2))], vec![Some(saved(&m, 2, 2))]];
            m.with_user_display_edit(true, |m| {
                m.model.range_clear_contents(&area(1, 2, 1, 2))?;
                m.model
                    .set_user_array_formula(0, 1, 2, 1, 2, "=ROW(A1:A2)+0")?;
                m.push_diff_list(vec![
                    Diff::RangeClearContents {
                        sheet: 0,
                        row: 1,
                        column: 2,
                        width: 1,
                        height: 2,
                        old_value: old_values,
                    },
                    Diff::SetArrayValue {
                        sheet: 0,
                        row: 1,
                        column: 2,
                        width: 1,
                        height: 2,
                        new_value: "=ROW(A1:A2)+0".into(),
                        old_values: vec![vec![None], vec![None]],
                    },
                ]);
                m.evaluate_if_not_paused();
                Ok(())
            })
            .unwrap();
        } else {
            m.set_user_input(0, 1, 2, "=SEQUENCE(2)").unwrap();
            m.set_user_input(0, 1, 2, "=SEQUENCE(2,1,1,1)").unwrap();
        }
        let anchor = saved(&m, 1, 2);
        let child = saved(&m, 2, 2);
        m.pause_evaluation();
        for _ in 0..2 {
            m.undo().unwrap();
            assert_eq!(value(&m, 1, 2), "1");
            assert_eq!(value(&m, 2, 2), "2");
            m.redo().unwrap();
            assert_eq!(saved(&m, 1, 2), anchor);
            assert_eq!(saved(&m, 2, 2), child);
            assert_eq!(value(&m, 2, 2), "2");
        }
    }
}
#[test]
fn nested_delete_then_recreate_retains_equal_results() {
    let mut m = UserModel::from_model(new_empty_model());
    m.set_user_input(0, 1, 2, "=2").unwrap();
    let old = m.model.workbook.worksheets[0]
        .cell(1, 2)
        .unwrap()
        .into_owned();
    m.with_user_display_edit(true, |m| {
        m.model.range_clear_contents(&area(1, 2, 1, 1))?;
        m.model.set_user_input(0, 1, 2, "=2".into())?;
        m.push_diff_list(vec![Diff::SetCellValue {
            sheet: 0,
            row: 1,
            column: 2,
            new_value: "=2".into(),
            old_value: Box::new(Some(old)),
        }]);
        m.evaluate_if_not_paused();
        Ok(())
    })
    .unwrap();
    m.pause_evaluation();
    m.undo().unwrap();
    assert_eq!(value(&m, 1, 2), "2");
    m.redo().unwrap();
    assert_eq!(value(&m, 1, 2), "2");
}
#[test]
fn calculation_and_style_writes_do_not_retain_equal_unrelated_payloads() {
    let mut raw = new_empty_model();
    raw._set("A1", "1");
    raw._set("B1", "=SEQUENCE(2)");
    for c in 10..=200 {
        raw.set_user_input(0, 1, c, "=7".into()).unwrap();
    }
    raw.evaluate();
    let mut m = UserModel::from_model(raw);
    m.set_user_input(0, 1, 1, "1").unwrap();
    assert_eq!(m.history.undo_stack.last().unwrap().display.cell_count(), 0);
    let index = m.model.evaluation_report().clone();
    let mut style = m.model.get_style_for_cell(0, 1, 10).unwrap();
    style.font.b = true;
    m.with_user_display_edit(true, |m| {
        let old = m.model.get_cell_style_or_none(0, 1, 10)?;
        m.model.set_cell_style(0, 1, 10, &style)?;
        m.push_diff_list(vec![Diff::SetCellStyle {
            sheet: 0,
            row: 1,
            column: 10,
            old_value: Box::new(old),
            new_value: Box::new(style),
        }]);
        Ok(())
    })
    .unwrap();
    assert_eq!(m.history.undo_stack.last().unwrap().display.cell_count(), 0);
    assert_eq!(m.model.evaluation_report(), &index);
}
#[test]
fn rejected_format_updates_do_not_create_dxfs_or_cleanup_links() {
    with_advancing_time(1_700_000_000_000, 1000, || {
        let mut m = new_empty_model();
        m._set("A1", "=HYPERLINK(\"https://example.com\",5)");
        m.evaluate();
        m.evaluate_incremental(&[]);
        let bytes = m.to_bytes();
        let report = m.evaluation_report().clone();
        let index = Arc::clone(m.dependency_index.as_ref().unwrap());
        let reads = clock_reads();
        let cf = m.evaluation.cf_evaluations;
        let visits = m.evaluation.display.link_cleanup_visits;
        for _ in 0..3 {
            assert!(m.update_conditional_formatting(0, 0, "B1", rule()).is_err());
            assert!(m
                .update_conditional_formatting(99, 0, "B1", rule())
                .is_err());
        }
        assert_eq!(m.to_bytes(), bytes);
        assert!(m.workbook.styles.dxfs.is_empty());
        assert_eq!(m.evaluation_report(), &report);
        assert!(Arc::ptr_eq(&index, m.dependency_index.as_ref().unwrap()));
        assert_eq!(clock_reads(), reads);
        assert_eq!(m.evaluation.cf_evaluations, cf);
        assert_eq!(m.evaluation.display.link_cleanup_visits, visits);
    });
}
#[test]
fn signed_array_fallbacks_use_the_supplied_full_style_context() {
    for input in ["50%", "$12.50", "10/17/2026"] {
        for range in [false, true] {
            for cross in [false, true] {
                for sign in ["+", "-"] {
                    let mut full = new_empty_model();
                    if cross {
                        full.new_sheet();
                    }
                    let sheet = u32::from(cross);
                    full.set_user_input(sheet, 1, 3, input.into()).unwrap();
                    let prefix = if cross { "Sheet2!" } else { "" };
                    let formula = if range {
                        format!("{sign}SUM({prefix}C1:C3)")
                    } else {
                        format!("{sign}{prefix}C1")
                    };
                    let mut expected = Model::from_bytes(&full.to_bytes(), "en").unwrap();
                    expected
                        .set_user_array_formula(0, 1, 2, 1, 1, &formula)
                        .unwrap();
                    let mut staged = Model::from_bytes(&full.to_bytes(), "en").unwrap();
                    staged.workbook.worksheets[sheet as usize]
                        .sheet_data
                        .remove_cell(1, 3);
                    staged
                        .set_user_array_formula_with_styles(
                            &area(1, 2, 1, 1),
                            &formula,
                            &|_, cell| full.get_style_for_cell(cell.sheet, cell.row, cell.column),
                        )
                        .unwrap();
                    assert_eq!(
                        staged.get_style_for_cell(0, 1, 2).unwrap().num_fmt,
                        expected.get_style_for_cell(0, 1, 2).unwrap().num_fmt
                    );
                    assert!(matches!(
                        staged.workbook.worksheets[0].cell(1, 2).as_deref(),
                        Some(Cell::CellFormula { .. })
                    ));
                }
            }
        }
    }
}
#[test]
fn failed_batches_format_retained_dependents_and_noop_rejections_do_no_work() {
    with_advancing_time(1_700_000_000_000, 1000, || {
        let mut m = UserModel::from_model(new_empty_model());
        m.set_user_input(0, 1, 1, "5").unwrap();
        m.set_user_input(0, 1, 2, "=A1*2").unwrap();
        m.set_user_input(0, 1, 3, "=HYPERLINK(\"https://example.com\",7)")
            .unwrap();
        m.add_conditional_formatting(0, "B1", rule()).unwrap();
        let cf = m.model.evaluation.cf_evaluations;
        let visits = m.model.evaluation.display.link_cleanup_visits;
        let bytes = m.to_bytes();
        assert!(m.apply_external_diffs(&[255]).is_err());
        assert_eq!(m.to_bytes(), bytes);
        assert_eq!(m.model.evaluation.cf_evaluations, cf);
        assert_eq!(m.model.evaluation.display.link_cleanup_visits, visits);
        let payload = bitcode::encode(&vec![QueueDiffs {
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
                    new_value: "9".into(),
                    old_value: Box::new(None),
                },
            ],
        }]);
        let reads = clock_reads();
        let evals = m.model.evaluation.full_evaluations;
        assert!(m.apply_external_diffs(&payload).is_err());
        assert_eq!(value(&m, 1, 1), "");
        assert_eq!(value(&m, 1, 2), "10");
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
        assert_eq!(m.model.evaluation.full_evaluations, evals);
        assert_eq!(clock_reads(), reads);
    });
}
#[test]
fn incremental_edits_borrow_without_clears_and_clear_tracking_scans_once() {
    let mut m = new_empty_model();
    m._set("A1", "1");
    m._set("A3", "2");
    m._set("B1", "=SUM(A1:A10)");
    m.evaluate();
    m.evaluate_incremental(&[]);
    m.evaluate_incremental(&[(0, 1, 1), (0, 1, 1)]);
    assert_eq!(m.evaluation.display.incremental_edit_allocations, 0);
    let visits = m.evaluation.display.clear_scan_visits;
    m.range_clear_contents(&area(1, 1, 1, 10)).unwrap();
    assert_eq!(m.evaluation.display.clear_scan_visits - visits, 2);
    assert!(m.fork_evaluation_session().is_err());
    m.evaluate_incremental(&[]);
    assert_eq!(m._get_text("B1"), "0");
    assert_eq!(m.evaluation.display.incremental_edit_allocations, 1);
    m.set_user_input(0, 1, 1, "3".into()).unwrap();
    m.evaluate_incremental(&[(0, 1, 1)]);
    m.set_user_input(0, 1, 1, "".into()).unwrap();
    m.evaluate_incremental(&[(0, 1, 1)]);
    assert_eq!(m._get_text("B1"), "0");
}
#[test]
fn pasted_cse_anchors_publish_one_final_style_diff() {
    let mut m = UserModel::from_model(new_empty_model());
    m.set_user_input(0, 1, 5, "50%").unwrap();
    m.set_user_array_formula(0, 1, 1, 1, 2, "=E1").unwrap();
    m.set_user_array_formula(0, 1, 7, 1, 2, "=A1*2").unwrap();
    m.flush_send_queue();
    m.set_selected_cell(1, 1).unwrap();
    m.set_selected_range(1, 1, 2, 1).unwrap();
    let cp = m.copy_to_clipboard().unwrap();
    m.set_selected_cell(1, 3).unwrap();
    m.paste_from_clipboard(0, cp.range, &cp.data, true).unwrap();
    let queue: Vec<QueueDiffs> = bitcode::decode(&m.flush_send_queue()).unwrap();
    let diffs: Vec<_> = queue.iter().flat_map(|q| &q.list).collect();
    assert_eq!(
        diffs
            .iter()
            .filter(|d| matches!(
                d,
                Diff::SetCellStyle {
                    sheet: 0,
                    row: 1,
                    column: 3,
                    ..
                }
            ))
            .count(),
        1
    );
    assert_eq!(
        diffs
            .iter()
            .filter(|d| matches!(
                d,
                Diff::SetCellStyle {
                    sheet: 0,
                    row: 1,
                    column: 7,
                    ..
                }
            ))
            .count(),
        1
    );
}

#[test]
fn invalid_iteration_wasm_fixture_has_expected_settings_and_round_trips() {
    let bytes = include_bytes!("../../tests/fixtures/invalid-iterations.ic");
    let workbook: Workbook = bitcode::decode(bytes).unwrap();
    assert!(workbook.settings.iterative_calculation.enabled);
    assert_eq!(
        workbook.settings.iterative_calculation.maximum_iterations,
        0
    );
    assert_eq!(bitcode::encode(&workbook).as_slice(), bytes);
    let mut model = Model::from_bytes(bytes, "en").unwrap();
    assert!(model.set_locale("fr").is_err());
    model.evaluate();
    assert_eq!(
        model.evaluation_report().failure,
        Some(CalculationFailure::InvalidSettings)
    );
}

#[test]
fn in_place_formula_identity_changes_retain_equal_cached_values_and_links() {
    for (old, new) in [
        ("=1+1", "=2"),
        (
            "=HYPERLINK(\"https://example.com\",1+1)",
            "=HYPERLINK(\"https://example.com\",2)",
        ),
    ] {
        let mut raw = new_empty_model();
        raw._set("B1", old);
        raw._set("C1", new);
        raw.evaluate();
        let index = raw.workbook.worksheets[0]
            .cell(1, 3)
            .unwrap()
            .get_formula()
            .unwrap();
        let mut m = UserModel::from_model(raw);
        let old_cell = saved(&m, 1, 2);
        m.with_user_display_edit(true, |m| {
            if let Some(Cell::CellFormula { f, .. }) = m.model.workbook.worksheets[0].cell_mut(1, 2)
            {
                *f = index;
            }
            m.push_diff_list(vec![Diff::SetCellValue {
                sheet: 0,
                row: 1,
                column: 2,
                new_value: new.into(),
                old_value: Box::new(Some(old_cell)),
            }]);
            m.evaluate_if_not_paused();
            Ok(())
        })
        .unwrap();
        let links = m.get_links_list(0).unwrap();
        m.pause_evaluation();
        m.undo().unwrap();
        m.redo().unwrap();
        assert_eq!(value(&m, 1, 2), "2");
        assert_eq!(m.get_links_list(0).unwrap(), links);
    }
}

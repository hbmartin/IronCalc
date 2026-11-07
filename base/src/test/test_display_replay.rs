#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use crate::{
    cf_types::CfRuleInput,
    expressions::types::Area,
    mock_time::{clock_reads, with_advancing_time},
    test::util::new_empty_model,
    types::{CalculationFailure, Cell, Dxf, DxfFont, FormulaValue, Workbook},
    UserModel,
};

fn area(row: i32, column: i32, width: i32, height: i32) -> Area {
    Area {
        sheet: 0,
        row,
        column,
        width,
        height,
    }
}
fn bold_rule(formula: &str) -> CfRuleInput {
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
fn grid(model: &UserModel) -> Vec<String> {
    (1..=8)
        .flat_map(|r| (1..=8).map(move |c| model.get_formatted_cell_value(0, r, c).unwrap()))
        .collect()
}

#[test]
fn structural_history_restores_equal_position_caches_after_replay() {
    for rows in [false, true] {
        for action in 0..3 {
            let mut raw = new_empty_model();
            for i in 1..=4 {
                raw.set_user_input(
                    0,
                    if rows { i } else { 1 },
                    if rows { 1 } else { i },
                    if rows { "=ROW()" } else { "=COLUMN()" }.into(),
                )
                .unwrap();
            }
            raw._set("F6", if rows { "=SUM(A1:A4)" } else { "=SUM(A1:D1)" });
            raw._set("G7", "=7");
            raw._set("G8", "=7");
            raw.evaluate();
            let mut model = UserModel::from_model(raw);
            let before = grid(&model);
            model
                .with_user_display_edit(true, |m| match (rows, action) {
                    (true, 0) => m.insert_rows(0, 1, 1),
                    (false, 0) => m.insert_columns(0, 1, 1),
                    (true, 1) => m.delete_rows(0, 2, 1),
                    (false, 1) => m.delete_columns(0, 2, 1),
                    (true, _) => m.move_rows_action(0, 2, 1, 2),
                    (false, _) => m.move_columns_action(0, 2, 1, 2),
                })
                .unwrap();
            let after = grid(&model);
            model.pause_evaluation();
            for _ in 0..2 {
                model.undo().unwrap();
                assert_eq!(grid(&model), before, "rows={rows}, action={action}");
                assert_eq!(
                    model.model.evaluation_report().failure,
                    Some(CalculationFailure::NotEvaluated)
                );
                let saved: Workbook = bitcode::decode(&model.to_bytes()).unwrap();
                for i in 1..=4 {
                    assert!(
                        matches!(saved.worksheets[0].cell(if rows {i} else {1},if rows {1} else {i}).as_deref(), Some(Cell::CellFormula {v: FormulaValue::Number(n), ..}) if *n==f64::from(i))
                    );
                }
                model.redo().unwrap();
                assert_eq!(grid(&model), after, "rows={rows}, action={action}");
            }
        }
    }
}

#[test]
fn structural_history_restores_spills_and_links_after_failed_calculation() {
    let mut raw = new_empty_model();
    raw._set("A1", "=SEQUENCE(3)");
    raw._set("C1", "=HYPERLINK(\"https://example.com\",\"link\")");
    raw.evaluate();
    raw.workbook.settings.iterative_calculation.enabled = true;
    raw.workbook
        .settings
        .iterative_calculation
        .maximum_iterations = 0;
    let mut model = UserModel::from_model(raw);
    let before = grid(&model);
    model.insert_rows(0, 1, 1).unwrap();
    let after = grid(&model);
    assert_eq!(
        model.model.evaluation_report().failure,
        Some(CalculationFailure::InvalidSettings)
    );
    model.pause_evaluation();
    model.undo().unwrap();
    assert_eq!(grid(&model), before);
    assert!(model.model.links.contains_key(&(0, 1, 3)));
    assert!(matches!(
        model.model.workbook.worksheets[0].cell(3, 1).as_deref(),
        Some(Cell::SpillCell { a: (1, 1), .. })
    ));
    model.redo().unwrap();
    assert_eq!(grid(&model), after);
    assert!(model.model.links.contains_key(&(0, 2, 3)));
    assert!(!model.model.links.contains_key(&(0, 1, 3)));
}

#[test]
fn autofill_respects_pause_and_invalidates_reports_in_both_directions() {
    for paused in [false, true] {
        for rows in [false, true] {
            with_advancing_time(1_700_000_000_000, 1000, || {
                let mut raw = new_empty_model();
                raw._set("A1", "=ROW()+COLUMN()");
                raw.evaluate();
                let mut model = UserModel::from_model(raw);
                if paused {
                    model.pause_evaluation();
                }
                let reads = clock_reads();
                let evaluations = model.model.evaluation.full_evaluations;
                if rows {
                    model.auto_fill_rows(&area(1, 1, 1, 1), 3).unwrap();
                } else {
                    model.auto_fill_columns(&area(1, 1, 1, 1), 3).unwrap();
                }
                assert_eq!(clock_reads(), reads + usize::from(!paused));
                assert_eq!(
                    model.model.evaluation.full_evaluations,
                    evaluations + usize::from(!paused)
                );
                assert_eq!(model.model.evaluation_report().converged, !paused);
                assert_eq!(model.model.fork_evaluation_session().is_ok(), !paused);
                if paused {
                    assert_eq!(
                        model.model.evaluation_report().failure,
                        Some(CalculationFailure::NotEvaluated)
                    );
                }
                model.evaluate();
                assert_eq!(
                    model
                        .get_formatted_cell_value(
                            0,
                            if rows { 3 } else { 1 },
                            if rows { 1 } else { 3 }
                        )
                        .unwrap(),
                    "4"
                );
            });
        }
    }
}

#[test]
fn every_merge_variant_moves_cached_results_and_dynamic_links_through_history() {
    for variant in 0..4 {
        for paused in [false, true] {
            with_advancing_time(1_700_000_000_000, 1000, || {
                let mut raw = new_empty_model();
                let source = if variant == 3 { (2, 1) } else { (1, 2) };
                raw.set_user_input(
                    0,
                    source.0,
                    source.1,
                    "=HYPERLINK(\"https://example.com\",\"link\")".into(),
                )
                .unwrap();
                raw.evaluate();
                let mut model = UserModel::from_model(raw);
                if paused {
                    model.pause_evaluation();
                }
                let reads = clock_reads();
                let evaluations = model.model.evaluation.full_evaluations;
                let formats = model.model.evaluation.cf_evaluations;
                match variant {
                    0 => model.merge_cells(&area(1, 1, 2, 1)),
                    1 => model.merge_cells_center(&area(1, 1, 2, 1)),
                    2 => model.merge_cells_across(&area(1, 1, 2, 2)),
                    _ => model.merge_cells_down(&area(1, 1, 2, 2)),
                }
                .unwrap();
                assert_eq!(clock_reads(), reads + usize::from(!paused));
                assert_eq!(
                    model.model.evaluation.full_evaluations,
                    evaluations + usize::from(!paused)
                );
                assert_eq!(model.model.evaluation.cf_evaluations, formats + 1);
                assert_eq!(model.get_formatted_cell_value(0, 1, 1).unwrap(), "link");
                assert!(model.model.links.contains_key(&(0, 1, 1)));
                assert!(!model.model.links.contains_key(&(0, source.0, source.1)));
                model.pause_evaluation();
                model.undo().unwrap();
                assert_eq!(
                    model
                        .get_formatted_cell_value(0, source.0, source.1)
                        .unwrap(),
                    "link"
                );
                assert!(model.model.links.contains_key(&(0, source.0, source.1)));
                model.redo().unwrap();
                assert_eq!(model.get_formatted_cell_value(0, 1, 1).unwrap(), "link");
                assert!(model.model.links.contains_key(&(0, 1, 1)));
                model.evaluate();
                assert!(model.model.evaluation_report().converged);
            });
        }
    }
}

#[test]
fn raw_merges_preserve_display_without_hidden_calculations() {
    for variant in 0..4 {
        let mut raw = new_empty_model();
        let source = if variant == 3 { (2, 1) } else { (1, 2) };
        raw.set_user_input(
            0,
            source.0,
            source.1,
            "=HYPERLINK(\"https://example.com\",\"link\")".into(),
        )
        .unwrap();
        raw.evaluate();
        let evaluations = raw.evaluation.full_evaluations;
        let formats = raw.evaluation.cf_evaluations;
        match variant {
            0 => raw.merge_cells(&area(1, 1, 2, 1)),
            1 => raw.merge_cells_center(&area(1, 1, 2, 1)),
            2 => raw.merge_cells_across(&area(1, 1, 2, 2)),
            _ => raw.merge_cells_down(&area(1, 1, 2, 2)),
        }
        .unwrap();
        assert_eq!(raw.evaluation.full_evaluations, evaluations);
        assert_eq!(raw.evaluation.cf_evaluations, formats + 1);
        assert_eq!(raw.get_formatted_cell_value(0, 1, 1).unwrap(), "link");
        assert!(raw.links.contains_key(&(0, 1, 1)));
        assert_eq!(
            raw.evaluation_report().failure,
            Some(CalculationFailure::NotEvaluated)
        );
    }
}

#[test]
fn formatting_edits_preserve_calculation_state_in_history_and_external_replay() {
    for paused in [false, true] {
        for failed in [false, true] {
            with_advancing_time(1_700_000_000_000, 1000, || {
                let mut raw = new_empty_model();
                raw._set("A1", "=RAND()");
                raw._set("B1", "=NOW()");
                raw._set("C1", "=HYPERLINK(\"https://example.com\",\"link\")");
                raw.add_conditional_formatting(0, "A1", bold_rule("TRUE"))
                    .unwrap();
                raw.add_conditional_formatting(0, "B1", bold_rule("FALSE"))
                    .unwrap();
                if failed {
                    raw._set("D1", "=D1");
                }
                raw.evaluate();
                if !failed {
                    raw.evaluate_incremental(&[]);
                }
                let mut peer =
                    UserModel::from_model(crate::Model::from_bytes(&raw.to_bytes(), "en").unwrap());
                peer.evaluate();
                let mut model = UserModel::from_model(raw);
                if paused {
                    model.pause_evaluation();
                    peer.pause_evaluation();
                }
                let report = model.model.evaluation_report().clone();
                let peer_report = peer.model.evaluation_report().clone();
                let index = model.model.dependency_index.clone();
                let values = grid(&model);
                let links = model.model.get_links_list(0).unwrap();
                let time = model.model.evaluation.display_timestamp;
                let reads = clock_reads();
                let evaluations = model.model.evaluation.full_evaluations;
                for operation in 0..4 {
                    let formats = model.model.evaluation.cf_evaluations;
                    match operation {
                        0 => model.add_conditional_formatting(0, "D2", bold_rule("TRUE")),
                        1 => model.update_conditional_formatting(0, 0, "A1", bold_rule("FALSE")),
                        2 => model.raise_conditional_formatting_priority(0, 0),
                        _ => model.delete_conditional_formatting(0, 1),
                    }
                    .unwrap();
                    assert_eq!(model.model.evaluation.cf_evaluations, formats + 1);
                    peer.apply_external_diffs(&model.flush_send_queue())
                        .unwrap();
                    assert_eq!(peer.model.evaluation_report(), &peer_report);
                    model.undo().unwrap();
                    model.redo().unwrap();
                    assert_eq!(model.model.evaluation_report(), &report);
                    assert_eq!(grid(&model), values);
                    assert_eq!(model.model.get_links_list(0).unwrap(), links);
                    assert_eq!(model.model.evaluation.display_timestamp, time);
                    if let Some(ref index) = index {
                        assert!(Arc::ptr_eq(
                            index,
                            model.model.dependency_index.as_ref().unwrap()
                        ));
                    }
                    assert_eq!(model.model.evaluation.full_evaluations, evaluations);
                    assert_eq!(clock_reads(), reads);
                }
            });
        }
    }
}

#[test]
fn geometry_and_style_history_skip_rule_evaluation_but_themes_refresh_once() {
    with_advancing_time(1_700_000_000_000, 1000, || {
        let mut raw = new_empty_model();
        raw._set("A1", "=RAND()");
        raw.add_conditional_formatting(0, "A1", bold_rule("TRUE"))
            .unwrap();
        raw.evaluate();
        raw.evaluate_incremental(&[]);
        let mut model = UserModel::from_model(raw);
        model.pause_evaluation();
        let report = model.model.evaluation_report().clone();
        let values = grid(&model);
        let reads = clock_reads();
        let formats = model.model.evaluation.cf_evaluations;
        let calculations = model.model.evaluation.full_evaluations;
        for operation in 0..6 {
            match operation {
                0 => model.set_columns_width(0, 1, 1, 30.0),
                1 => model.set_rows_height(0, 1, 1, 40.0),
                2 => model.update_range_style(&area(1, 1, 1, 1), "font.b", "true"),
                3 => model.set_show_grid_lines(0, false),
                4 => model.set_frozen_rows_count(0, 1),
                _ => model.set_frozen_columns_count(0, 1),
            }
            .unwrap();
            model.undo().unwrap();
            model.redo().unwrap();
            assert_eq!(model.model.evaluation.cf_evaluations, formats);
            assert_eq!(model.model.evaluation.full_evaluations, calculations);
            assert_eq!(model.model.evaluation_report(), &report);
            assert_eq!(grid(&model), values);
            assert_eq!(clock_reads(), reads);
        }
        model.set_theme(model.get_theme());
        assert_eq!(model.model.evaluation.cf_evaluations, formats + 1);
        model.undo().unwrap();
        assert_eq!(model.model.evaluation.cf_evaluations, formats + 2);
        assert_eq!(clock_reads(), reads);
    });
}

#[test]
fn volatile_history_shares_payload_across_undo_redo_and_conflict_pruning_is_copy_on_write() {
    let mut raw = new_empty_model();
    for row in 1..=200 {
        raw.set_user_input(0, row, 1, "=RAND()".into()).unwrap();
    }
    raw.evaluate();
    let mut peer = UserModel::from_model(crate::Model::from_bytes(&raw.to_bytes(), "en").unwrap());
    let mut model = UserModel::from_model(raw);
    model.set_user_input(0, 1, 2, "1").unwrap();
    let original = Arc::clone(&model.history.undo_stack.last().unwrap().display);
    assert_eq!(original.cell_count(), 200);
    model.pause_evaluation();
    model.undo().unwrap();
    assert!(Arc::ptr_eq(
        &original,
        &model.history.redo_stack.last().unwrap().display
    ));
    model.redo().unwrap();
    assert!(Arc::ptr_eq(
        &original,
        &model.history.undo_stack.last().unwrap().display
    ));
    peer.set_user_input(0, 1, 1, "=100").unwrap();
    model.resume_evaluation();
    model
        .apply_external_diffs(&peer.flush_send_queue())
        .unwrap();
    model.pause_evaluation();
    let pruned = &model.history.undo_stack.last().unwrap().display;
    assert!(!Arc::ptr_eq(&original, pruned));
    assert_eq!(original.cell_count(), 200);
    assert_eq!(pruned.cell_count(), 199);
    model.undo().unwrap();
    assert_eq!(model.get_formatted_cell_value(0, 1, 1).unwrap(), "100");
}

#[cfg(panic = "unwind")]
#[test]
fn panics_release_nested_user_and_raw_scopes_and_finalization_journals() {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    for stage in 0..3 {
        let mut model = UserModel::from_model(new_empty_model());
        model.set_user_input(0, 1, 1, "=1").unwrap();
        let result = catch_unwind(AssertUnwindSafe(|| {
            if stage == 0 {
                let _: Result<(), String> = model.with_user_display_edit(true, |m| {
                    m.model.with_display_batch(|raw| {
                        raw.with_structural_display_edit(crate::Model::display_identity, |raw| {
                            raw._set("B1", "=2");
                            panic!("nested mutation")
                        })
                    })
                });
            } else {
                let mut scope = crate::display::DisplayEditGuard::new(&mut model, false);
                scope.target().model.start_display_journal();
                scope.target().model._set("B1", "=2");
                scope.target().model.evaluate();
                scope.finish(true);
                if stage == 1 {
                    panic!("before journal publication");
                }
                let _delta = scope.target().model.take_display_journal();
                panic!("after journal publication");
            }
        }));
        assert!(result.is_err());
        assert_eq!(model.model.evaluation.display.edit_depth, 0);
        assert!(!model.model.evaluation.display.calculation_requested);
        assert!(!model.model.evaluation.display.formatting_requested);
        assert_eq!(
            model.model.evaluation_report().failure,
            Some(CalculationFailure::NotEvaluated)
        );
        model.set_user_input(0, 1, 3, "=B1+1").unwrap();
        assert_eq!(model.get_formatted_cell_value(0, 1, 3).unwrap(), "3");
        assert!(model.model.fork_evaluation_session().is_ok());
    }
}

#[test]
fn rejected_context_batches_can_be_retried_after_resume_or_settings_repair() {
    for invalid in [false, true] {
        with_advancing_time(1_700_000_000_000, 1000, || {
            let mut sender = UserModel::from_model(new_empty_model());
            sender.set_user_input(0, 1, 1, "42").unwrap();
            sender.set_timezone("Europe/Paris").unwrap();
            let payload = sender.flush_send_queue();
            let mut raw = new_empty_model();
            raw.evaluate();
            if invalid {
                raw.workbook.settings.iterative_calculation.enabled = true;
                raw.workbook
                    .settings
                    .iterative_calculation
                    .maximum_iterations = 0;
            }
            let mut receiver = UserModel::from_model(raw);
            if !invalid {
                receiver.pause_evaluation();
            }
            let before = receiver.to_bytes();
            let report = receiver.model.evaluation_report().clone();
            let reads = clock_reads();
            assert!(receiver.apply_external_diffs(&payload).is_err());
            assert_eq!(receiver.to_bytes(), before);
            assert_eq!(receiver.model.evaluation_report(), &report);
            assert_eq!(clock_reads(), reads);
            assert!(!receiver.can_undo());
            if invalid {
                receiver
                    .set_iterative_calculation(false, 100, 0.001)
                    .unwrap();
            } else {
                receiver.resume_evaluation();
            }
            receiver.apply_external_diffs(&payload).unwrap();
            assert_eq!(receiver.get_timezone(), "Europe/Paris");
            assert_eq!(receiver.get_formatted_cell_value(0, 1, 1).unwrap(), "42");
        });
    }
}

#[test]
fn structural_replay_retains_equal_dynamic_links_at_moved_positions() {
    let mut raw = new_empty_model();
    for row in 1..=3 {
        raw.set_user_input(
            0,
            row,
            1,
            "=HYPERLINK(\"https://example.com\",\"same\")".into(),
        )
        .unwrap();
    }
    raw.evaluate();
    let mut model = UserModel::from_model(raw);
    model.insert_rows(0, 1, 1).unwrap();
    model.pause_evaluation();
    model.undo().unwrap();
    for row in 1..=3 {
        assert!(model.model.links.contains_key(&(0, row, 1)));
    }
    assert!(!model.model.links.contains_key(&(0, 4, 1)));
    model.redo().unwrap();
    assert!(!model.model.links.contains_key(&(0, 1, 1)));
    for row in 2..=4 {
        assert!(model.model.links.contains_key(&(0, row, 1)));
    }
}

#[cfg(all(debug_assertions, panic = "unwind"))]
#[test]
fn panic_inside_final_calculation_releases_the_user_scope() {
    let mut model = UserModel::from_model(new_empty_model());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Overflow the advancing test clock when the outer edit starts its
        // final calculation, after the mutation and history have completed.
        with_advancing_time(i64::MAX, 1, || model.set_user_input(0, 1, 1, "=1").unwrap());
    }));
    assert!(result.is_err());
    assert_eq!(model.model.evaluation.display.edit_depth, 0);
    assert_eq!(
        model.model.evaluation_report().failure,
        Some(CalculationFailure::NotEvaluated)
    );
    model.set_user_input(0, 1, 2, "=A1+1").unwrap();
    assert_eq!(model.get_formatted_cell_value(0, 1, 2).unwrap(), "2");
}

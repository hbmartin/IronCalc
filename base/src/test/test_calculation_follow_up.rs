#![allow(clippy::unwrap_used, clippy::panic)]

use crate::{
    cell::CellValue,
    cf_types::CfRuleInput,
    mock_time::{clock_reads, with_advancing_time},
    test::util::new_empty_model,
    types::{CalculationFailure, Cell, Dxf, DxfFont, FormulaValue},
    Model, UserModel,
};

fn number(model: &Model, cell: &str) -> f64 {
    match model
        .get_cell_value_by_ref(&format!("Sheet1!{cell}"))
        .unwrap()
    {
        CellValue::Number(value) => value,
        other => panic!("{cell}: {other:?}"),
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

fn bold_at(model: &Model, row: i32, column: i32) -> bool {
    model
        .get_extended_style_for_cell(0, row, column)
        .unwrap()
        .style
        .font
        .b
}

fn assert_invalid_settings(model: &Model) {
    let report = model.evaluation_report();
    assert_eq!(report.failure, Some(CalculationFailure::InvalidSettings));
    assert!(!report.converged);
    assert_eq!(report.iterations, 0);
}

fn clock_model() -> Model<'static> {
    let mut model = new_empty_model();
    model._set("A1", "=NOW()");
    model._set("B1", "1");
    model._set("C1", "=HYPERLINK(\"https://example.com\",\"link\")");
    model
        .add_conditional_formatting(0, "B1", bold_rule("NOW()=$A$1"))
        .unwrap();
    model
}

fn assert_clock_format_matches(model: &Model) {
    assert!(bold_at(model, 1, 2));
}

#[test]
fn switch_preserves_result_caches_but_requires_a_new_report() {
    for enabled in [false, true] {
        let mut model = clock_model();
        model
            .set_iterative_calculation(enabled, 100, 0.001)
            .unwrap();
        if !enabled {
            assert!(model.evaluate_incremental(&[]).is_some());
        }
        let index = model.dependency_index.clone();
        let cells = model.evaluation.cells.clone();
        let timestamp = model.evaluation.display_timestamp;
        let links = model.get_links_list(0).unwrap();
        let bytes = model.to_bytes();
        let full_evaluations = model.evaluation.full_evaluations;
        for required in [false, true] {
            model.set_require_two_iterative_passes(required);
            assert_eq!(
                model.evaluation_report().failure,
                Some(CalculationFailure::NotEvaluated)
            );
            assert_eq!(model.evaluation_report().iterations, 0);
            assert!(!model.evaluation_report().converged);
            assert_eq!(model.evaluation.cells, cells);
            match (model.dependency_index.as_ref(), index.as_ref()) {
                (Some(actual), Some(expected)) => assert!(std::sync::Arc::ptr_eq(actual, expected)),
                (None, None) => {}
                _ => panic!("dependency index changed"),
            }
            assert_eq!(model.evaluation.display_timestamp, timestamp);
            assert_eq!(model.get_links_list(0).unwrap(), links);
            assert_eq!(model.to_bytes(), bytes);
            assert_clock_format_matches(&model);
            assert!(model.fork_evaluation_session().is_err());
            model.set_require_two_iterative_passes(required);
            assert_eq!(model.evaluation.full_evaluations, full_evaluations);
        }
        assert!(model.evaluate_incremental_headless(&[]).is_none());
        assert!(model.evaluation_report().converged);
        assert_eq!(model.evaluation_report().failure, None);
        assert!(model.fork_evaluation_session().is_ok());
    }
}

#[test]
fn user_switch_preserves_display_while_paused_with_iteration_on_or_off() {
    for enabled in [false, true] {
        let mut raw = clock_model();
        raw.set_iterative_calculation(enabled, 100, 0.001).unwrap();
        let mut model = UserModel::from_model(raw);
        let empty_queue = model.flush_send_queue();
        model.pause_evaluation();
        let evaluations = model.model.evaluation.full_evaluations;
        model.set_require_two_iterative_passes(false);
        assert_clock_format_matches(&model.model);
        assert_eq!(model.model.get_links_list(0).unwrap().len(), 1);
        assert_eq!(model.model.evaluation.full_evaluations, evaluations);
        assert_eq!(
            model.model.evaluation_report().failure,
            Some(CalculationFailure::NotEvaluated)
        );
        assert!(!model.can_undo());
        assert_eq!(model.flush_send_queue(), empty_queue);
        model.resume_evaluation();
        // Resuming itself does not calculate; a changed switch evaluates once.
        let evaluations = model.model.evaluation.full_evaluations;
        model.set_require_two_iterative_passes(true);
        assert_eq!(model.model.evaluation.full_evaluations, evaluations + 1);
        assert_clock_format_matches(&model.model);
        assert!(model.model.evaluation_report().converged);
        assert!(!model.can_undo());
        assert_eq!(model.flush_send_queue(), empty_queue);
    }
}

#[test]
fn result_affecting_limits_invalidate_only_the_report_and_force_reevaluation() {
    for lambda_limit in [false, true] {
        let mut model = clock_model();
        model._set("D1", "=LAMBDA(x,LAMBDA(y,y+1)(x))(1)");
        model.evaluate();
        assert!(model.evaluation_report().converged);
        assert!(model.evaluate_incremental(&[]).is_some());
        let index = model.dependency_index.clone().unwrap();
        let cells = model.evaluation.cells.clone();
        let links = model.get_links_list(0).unwrap();
        let display = model.evaluation.display_timestamp;
        let bytes = model.to_bytes();
        let evaluations = model.evaluation.full_evaluations;
        let original_limit = if lambda_limit {
            let original = model.get_max_lambda_depth();
            model.set_max_lambda_depth(1);
            original
        } else {
            let original = model.get_max_evaluation_stack();
            model.set_max_evaluation_stack(1);
            original
        };
        assert_eq!(
            model.evaluation_report().failure,
            Some(CalculationFailure::NotEvaluated)
        );
        assert!(!model.evaluation_report().converged);
        assert_eq!(model.evaluation_report().iterations, 0);
        assert_eq!(model.evaluation.cells, cells);
        assert!(std::sync::Arc::ptr_eq(
            model.dependency_index.as_ref().unwrap(),
            &index
        ));
        assert_eq!(model.get_links_list(0).unwrap(), links);
        assert_eq!(model.evaluation.display_timestamp, display);
        assert_eq!(model.to_bytes(), bytes);
        assert_eq!(model.evaluation.full_evaluations, evaluations);
        assert_eq!(number(&model, "D1"), 2.0);
        assert_clock_format_matches(&model);
        assert!(model.fork_evaluation_session().is_err());
        assert!(model.evaluate_incremental_headless(&[]).is_none());
        assert_eq!(
            model.evaluation_report().failure,
            Some(CalculationFailure::RecursionLimit)
        );
        assert!(!model.evaluation_report().converged);
        assert!(model.fork_evaluation_session().is_err());
        let failed = model.evaluation_report().clone();
        if lambda_limit {
            model.set_max_lambda_depth(1);
            model.set_max_lambda_depth(0);
            assert_eq!(model.get_max_lambda_depth(), 1);
        } else {
            model.set_max_evaluation_stack(1);
        }
        assert_eq!(model.evaluation_report(), &failed);
        if lambda_limit {
            model.set_max_lambda_depth(original_limit);
        } else {
            model.set_max_evaluation_stack(original_limit);
        }
        assert_eq!(
            model.evaluation_report().failure,
            Some(CalculationFailure::NotEvaluated)
        );
        assert!(model.evaluate_incremental(&[]).is_none());
        assert!(model.evaluation_report().converged);
        assert_eq!(number(&model, "D1"), 2.0);
        assert_clock_format_matches(&model);
        assert!(model.fork_evaluation_session().is_ok());
        let successful = model.evaluation_report().clone();
        if lambda_limit {
            model.set_max_lambda_depth(original_limit);
        } else {
            model.set_max_evaluation_stack(original_limit);
        }
        assert_eq!(model.evaluation_report(), &successful);
    }
}

#[test]
fn cell_evaluation_depth_changes_retain_reports_and_results() {
    let mut model = clock_model();
    model._set("D1", "=D2+1");
    model._set("D2", "=D3+1");
    model._set("D3", "1");
    model.evaluate();
    let report = model.evaluation_report().clone();
    let cells = model.evaluation.cells.clone();
    let evaluations = model.evaluation.full_evaluations;
    model.set_max_evaluation_depth(1);
    model.set_max_evaluation_depth(0);
    assert_eq!(model.get_max_evaluation_depth(), 1);
    assert_eq!(model.evaluation_report(), &report);
    assert_eq!(model.evaluation.cells, cells);
    assert_eq!(model.evaluation.full_evaluations, evaluations);
    assert_eq!(number(&model, "D1"), 3.0);
    assert!(model.fork_evaluation_session().is_ok());
    model.evaluate();
    assert_eq!(model.evaluation_report(), &report);
    assert_eq!(number(&model, "D1"), 3.0);
    assert_clock_format_matches(&model);
}

#[test]
fn two_pass_option_controls_initial_comparison_without_changing_tolerance() {
    for required in [true, false] {
        for (delta, tolerance, converged) in [
            (0.0001, 0.001, true),
            (0.001, 0.001, false),
            (0.0001, 0.00001, false),
            (0.0001, 0.0, false),
            (0.0, 0.0, true),
        ] {
            let mut model = new_empty_model();
            assert!(model.get_require_two_iterative_passes());
            model.set_require_two_iterative_passes(required);
            model._set("A1", &format!("=A1+{delta}"));
            model.set_iterative_calculation(true, 5, tolerance).unwrap();
            let report = model.evaluation_report();
            assert_eq!(
                report.converged, converged,
                "{required}, {delta}, {tolerance}"
            );
            let passes = if converged {
                if required {
                    2
                } else {
                    1
                }
            } else {
                5
            };
            assert_eq!(report.iterations, passes);
            assert!((number(&model, "A1") - delta * f64::from(passes)).abs() < 1e-12);
        }
    }
}

#[test]
fn single_pass_budget_cannot_establish_default_circular_convergence() {
    let mut model = new_empty_model();
    model._set("A1", "=A1*0");
    model.set_iterative_calculation(true, 1, 0.001).unwrap();
    assert!(!model.evaluation_report().converged);
    assert_eq!(
        model.evaluation_report().failure,
        Some(CalculationFailure::IterationLimit)
    );
    assert_eq!(model.evaluation_report().iterations, 1);
    model.set_require_two_iterative_passes(false);
    assert_eq!(model.evaluation_report().iterations, 0);
    model.evaluate_headless();
    assert!(model.evaluation_report().converged);
    model.set_require_two_iterative_passes(true);
    model._set("A1", "=42");
    model.evaluate_headless();
    assert!(model.evaluation_report().converged);
    assert_eq!(model.evaluation_report().iterations, 1);
}

#[test]
fn option_is_runtime_configuration_and_unchanged_values_are_noops() {
    let mut model = new_empty_model();
    model._set("A1", "=A1/2+10");
    model.set_require_two_iterative_passes(false);
    model.set_iterative_calculation(true, 100, 1e-9).unwrap();
    let report = model.evaluation_report().clone();
    model.set_require_two_iterative_passes(false);
    assert_eq!(model.evaluation_report(), &report);
    let mut fork = model.fork_evaluation_session().unwrap();
    assert!(!fork.get_require_two_iterative_passes());
    fork.insert_rows(0, 1, 1).unwrap();
    assert!(!fork.get_require_two_iterative_passes());
    assert!(!fork.evaluation_report().converged);
    let restored = Model::from_bytes(&model.to_bytes(), "en").unwrap();
    assert!(restored.get_require_two_iterative_passes());
}

#[test]
fn user_option_respects_pause_without_history_or_diffs() {
    let mut model = UserModel::from_model(new_empty_model());
    model.set_user_input(0, 1, 1, "=A1/2+10").unwrap();
    model.set_iterative_calculation(true, 100, 1e-9).unwrap();
    model.flush_send_queue();
    let before_queue = model.flush_send_queue();
    let before_evaluations = model.model.evaluation.full_evaluations;
    model.set_require_two_iterative_passes(false);
    assert_eq!(
        model.model.evaluation.full_evaluations,
        before_evaluations + 1
    );
    assert_eq!(model.flush_send_queue(), before_queue);
    model.set_require_two_iterative_passes(false);
    assert_eq!(
        model.model.evaluation.full_evaluations,
        before_evaluations + 1
    );
    model.undo().unwrap(); // Undo settings, not the runtime option.
    assert!(!model.get_iterative_calculation().enabled);
    assert!(!model.get_require_two_iterative_passes());
    model.redo().unwrap();
    assert!(!model.get_require_two_iterative_passes());
    model.pause_evaluation();
    let before_evaluations = model.model.evaluation.full_evaluations;
    model.set_require_two_iterative_passes(true);
    assert_eq!(model.model.evaluation.full_evaluations, before_evaluations);
    assert!(!model.model.evaluation_report().converged);
    assert_eq!(model.model.evaluation_report().iterations, 0);
    model.evaluate();
    assert!(model.model.evaluation_report().converged);
    assert!(UserModel::from_bytes(&model.to_bytes(), "en")
        .unwrap()
        .get_require_two_iterative_passes());
}

#[test]
fn date_cycles_share_one_instant_across_passes_and_indirect_reads() {
    for source in ["=TODAY()", "=NOW()", "=NOW(\"Pacific/Kiritimati\")"] {
        for cycle in [
            "=D1/2+A1",
            "=D1/2+SUM(A1:A2)",
            "=D1/2+INDIRECT(\"A1\")",
            "=D1/2+Input",
        ] {
            let mut model = new_empty_model();
            model
                .new_defined_name("Input", None, "Sheet1!$A$1")
                .unwrap();
            model._set("A1", source);
            model._set("A2", "0");
            model._set("D1", cycle);
            with_advancing_time(1679356799000, 86_400_000, || {
                model.set_iterative_calculation(true, 100, 1e-9).unwrap();
                assert!(
                    model.evaluation_report().converged,
                    "{source}, {cycle}: {:?}",
                    model.evaluation_report()
                );
                assert!((number(&model, "D1") - 2.0 * number(&model, "A1")).abs() < 2e-9);
                assert_eq!(clock_reads(), 1);
            });
        }
    }
}

#[test]
fn clock_refreshes_across_incremental_calls_and_full_fallback() {
    let mut model = new_empty_model();
    model._set("A1", "=NOW()");
    model._set("A2", "=TODAY()");
    model._set("B1", "=NOW(\"Pacific/Kiritimati\")");
    with_advancing_time(1679356799000, 86_400_000, || {
        model.evaluate();
        let first = number(&model, "A1");
        assert_eq!(number(&model, "A2"), first.floor());
        assert!((number(&model, "B1") - first - 14.0 / 24.0).abs() < 1e-10);
        assert_eq!(clock_reads(), 1);
        assert!(model.evaluate_incremental(&[]).is_some());
        assert_eq!(number(&model, "A1"), first + 1.0);
        assert_eq!(clock_reads(), 2);
        model._set("C1", "=SEQUENCE(2)");
        assert!(model.evaluate_incremental(&[(0, 1, 3)]).is_none());
        assert_eq!(number(&model, "A1"), first + 2.0);
        assert_eq!(clock_reads(), 3);
        let mut fork = model.fork_evaluation_session().unwrap();
        fork.evaluate_headless();
        assert_eq!(number(&fork, "A1"), first + 3.0);
        assert_eq!(clock_reads(), 4);
    });
}

#[test]
fn clock_survives_restarts_and_cell_chain_replay() {
    let mut model = new_empty_model();
    model.set_max_evaluation_depth(1);
    model._set("A1", "=A1/2+B1");
    model._set("B1", "=LET(x,C2,IF(x=0,RAND(),NOW()))");
    model._set("C1", "=SEQUENCE(2)");
    with_advancing_time(1679356799000, 1000, || {
        model.set_iterative_calculation(true, 100, 1e-9).unwrap();
        assert!(
            model.evaluation_report().converged,
            "{:?}",
            model.evaluation_report()
        );
        assert!(model.evaluation.restarts_in_last_evaluation > 0);
        assert_eq!(clock_reads(), 1);
    });
}

#[test]
fn conditional_formats_share_the_formula_calculation_timestamp() {
    let mut model = clock_model();
    with_advancing_time(1679356799000, 86_400_000, || {
        model.evaluate();
        assert_eq!(clock_reads(), 1);
        assert_clock_format_matches(&model);
        assert!(model.evaluate_incremental(&[]).is_some());
        assert_eq!(clock_reads(), 2);
        assert_clock_format_matches(&model);
    });
}

#[test]
fn clock_is_released_on_early_return() {
    let mut model = new_empty_model();
    model._set("A1", "=NOW()");
    model.workbook.settings.iterative_calculation.enabled = true;
    model
        .workbook
        .settings
        .iterative_calculation
        .maximum_iterations = 0;
    with_advancing_time(1679356799000, 1000, || {
        model.evaluate_headless();
        assert_eq!(clock_reads(), 1);
        assert!(model.evaluation.calculation_time.lock().unwrap().is_none());
        model.workbook.settings.iterative_calculation.enabled = false;
        model.evaluate_headless();
        assert_eq!(clock_reads(), 2);
    });
}

#[test]
#[cfg(panic = "unwind")]
fn clock_is_released_on_panic() {
    let model = new_empty_model();
    let display = model.evaluation.display_timestamp;
    with_advancing_time(1679356799000, 1000, || {
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _clock = model.start_calculation_clock();
            panic!("test clock cleanup");
        }))
        .is_err());
        assert!(model.evaluation.calculation_time.lock().unwrap().is_none());
        assert_eq!(model.evaluation.display_timestamp, display);
        let _clock = model.start_calculation_clock();
        assert_eq!(clock_reads(), 2);
    });
}

#[test]
#[cfg(all(debug_assertions, panic = "unwind"))]
#[should_panic(expected = "date evaluation requires an active clock scope")]
fn missing_clock_scope_is_diagnosed() {
    new_empty_model().calculation_timestamp_ms();
}

#[test]
fn redraws_retain_time_and_headless_calculations_refresh_it() {
    let mut model = clock_model();
    with_advancing_time(1679356799000, 86_400_000, || {
        model.evaluate();
        let first = number(&model, "A1");
        for _ in 0..2 {
            model.set_theme(model.get_theme());
            model.evaluate_conditional_formatting();
            assert_clock_format_matches(&model);
            assert_eq!(number(&model, "A1"), first);
            assert_eq!(clock_reads(), 1);
        }
        let mut fork = model.fork_evaluation_session().unwrap();
        assert!(fork.evaluation.calculation_time.lock().unwrap().is_none());
        fork.evaluate_conditional_formatting();
        assert_clock_format_matches(&fork);
        assert_eq!(clock_reads(), 1);
        fork.evaluate_headless();
        fork.evaluate_conditional_formatting();
        assert_clock_format_matches(&fork);
        assert_eq!(number(&fork, "A1"), first + 1.0);
        model.evaluate_incremental_headless(&[]);
        model.evaluate_conditional_formatting();
        assert_clock_format_matches(&model);
        assert_eq!(number(&model, "A1"), first + 2.0);
        assert_eq!(clock_reads(), 3);
    });
}

#[test]
fn reload_establishes_an_on_demand_display_snapshot_without_a_report() {
    let mut model = clock_model();
    with_advancing_time(1679356799000, 86_400_000, || {
        model.evaluate();
        let bytes = model.to_bytes();
        let first = number(&model, "A1");
        let mut restored = Model::from_bytes(&bytes, "en").unwrap();
        assert_eq!(
            restored.evaluation_report().failure,
            Some(CalculationFailure::NotEvaluated)
        );
        assert_eq!(restored.evaluation_report().iterations, 0);
        assert_eq!(number(&restored, "A1"), first + 1.0);
        assert_clock_format_matches(&restored);
        restored.set_theme(restored.get_theme());
        restored.evaluate_conditional_formatting();
        assert_eq!(clock_reads(), 2);
        assert_clock_format_matches(&restored);
        restored.evaluate();
        assert_eq!(number(&restored, "A1"), first + 2.0);
        assert!(restored.evaluation_report().converged);
        assert_eq!(restored.evaluation_report().failure, None);
        assert_eq!(clock_reads(), 3);
    });
}

#[test]
fn unsuccessful_passes_refresh_display_but_invalid_settings_preserve_it() {
    let mut model = clock_model();
    model._set("D1", "=D1+1");
    model.workbook.settings.iterative_calculation.enabled = true;
    model
        .workbook
        .settings
        .iterative_calculation
        .maximum_iterations = 1;
    with_advancing_time(1679356799000, 86_400_000, || {
        model.evaluate();
        assert_eq!(
            model.evaluation_report().failure,
            Some(CalculationFailure::IterationLimit)
        );
        let first = number(&model, "A1");
        model.set_theme(model.get_theme());
        assert_clock_format_matches(&model);
        assert_eq!(clock_reads(), 1);
        model.evaluate_headless();
        model.evaluate_conditional_formatting();
        assert_eq!(number(&model, "A1"), first + 1.0);
        assert_clock_format_matches(&model);
        let display = model.evaluation.display_timestamp;
        model
            .workbook
            .settings
            .iterative_calculation
            .maximum_iterations = 0;
        model.evaluate();
        assert_eq!(
            model.evaluation_report().failure,
            Some(CalculationFailure::InvalidSettings)
        );
        assert_eq!(model.evaluation.display_timestamp, display);
        assert_clock_format_matches(&model);
        assert!(model.evaluate_incremental(&[]).is_none());
        assert_eq!(model.evaluation.display_timestamp, display);
        model.set_theme(model.get_theme());
        assert_clock_format_matches(&model);
        assert_eq!(clock_reads(), 4);
    });
}

#[test]
fn invalid_settings_rebuild_current_rules_at_the_retained_display_time() {
    for incremental in [false, true] {
        let mut model = clock_model();
        model
            .add_conditional_formatting(0, "E1", bold_rule("TRUE()"))
            .unwrap();
        with_advancing_time(1679356799000, 86_400_000, || {
            model.evaluate();
            let first = number(&model, "A1");
            let display = model.evaluation.display_timestamp;
            assert!(bold_at(&model, 1, 5));
            model.workbook.settings.iterative_calculation.enabled = true;
            model
                .workbook
                .settings
                .iterative_calculation
                .maximum_iterations = 0;
            model
                .update_conditional_formatting(0, 0, "B1:B2", bold_rule("NOW()=$A$1"))
                .unwrap();
            model.delete_conditional_formatting(0, 1).unwrap();
            model
                .add_conditional_formatting(0, "D1", bold_rule("TRUE()"))
                .unwrap();
            if incremental {
                assert!(model.evaluate_incremental(&[]).is_none());
            } else {
                model.evaluate();
            }
            assert_invalid_settings(&model);
            assert_eq!(number(&model, "A1"), first);
            assert_eq!(model.evaluation.display_timestamp, display);
            assert_clock_format_matches(&model);
            assert!(bold_at(&model, 2, 2));
            assert!(bold_at(&model, 1, 4));
            assert!(!bold_at(&model, 1, 5));
            assert_eq!(clock_reads(), 2);
            assert!(model.evaluation.calculation_time.lock().unwrap().is_none());
            model.set_theme(model.get_theme());
            model.evaluate_conditional_formatting();
            assert_invalid_settings(&model);
            assert_clock_format_matches(&model);
            assert_eq!(clock_reads(), 2);
            model.workbook.settings.iterative_calculation.enabled = false;
            if incremental {
                assert!(model.evaluate_incremental(&[]).is_none());
            } else {
                model.evaluate();
            }
            assert!(model.evaluation_report().converged);
            assert_eq!(model.evaluation_report().failure, None);
            assert_eq!(number(&model, "A1"), first + 2.0);
            assert_clock_format_matches(&model);
            assert_eq!(clock_reads(), 3);
        });
    }
}

#[test]
fn invalid_loaded_workbooks_refresh_formatting_after_edits_and_history() {
    for through_bytes in [false, true] {
        let mut raw = new_empty_model();
        raw._set("A1", "5");
        raw.add_conditional_formatting(0, "B1:B10", bold_rule("$A$1>3"))
            .unwrap();
        raw.workbook.settings.iterative_calculation.enabled = true;
        raw.workbook
            .settings
            .iterative_calculation
            .maximum_iterations = 0;
        let loaded = if through_bytes {
            Model::from_bytes(&raw.to_bytes(), "en").unwrap()
        } else {
            Model::from_workbook(raw.workbook, "en").unwrap()
        };
        assert_eq!(
            loaded.evaluation_report().failure,
            Some(CalculationFailure::NotEvaluated)
        );
        assert!(bold_at(&loaded, 1, 2));
        let mut model = UserModel::from_model(loaded);
        model.insert_rows(0, 1, 1).unwrap();
        assert_invalid_settings(&model.model);
        assert!(!bold_at(&model.model, 1, 2));
        assert!(bold_at(&model.model, 2, 2));
        assert!(bold_at(&model.model, 11, 2));
        model.undo().unwrap();
        assert!(bold_at(&model.model, 1, 2));
        assert!(!bold_at(&model.model, 11, 2));
        model.redo().unwrap();
        assert!(!bold_at(&model.model, 1, 2));
        assert!(bold_at(&model.model, 2, 2));
        model
            .add_conditional_formatting(0, "C1", bold_rule("$A$2>3"))
            .unwrap();
        assert!(bold_at(&model.model, 1, 3));
        model.undo().unwrap();
        assert!(!bold_at(&model.model, 1, 3));
        model.redo().unwrap();
        assert!(bold_at(&model.model, 1, 3));
        model
            .update_conditional_formatting(0, 1, "C1", bold_rule("$A$2<3"))
            .unwrap();
        assert!(!bold_at(&model.model, 1, 3));
        model.undo().unwrap();
        assert!(bold_at(&model.model, 1, 3));
        model.redo().unwrap();
        assert!(!bold_at(&model.model, 1, 3));
        model.delete_conditional_formatting(0, 0).unwrap();
        assert!(!bold_at(&model.model, 2, 2));
        model.undo().unwrap();
        assert!(bold_at(&model.model, 2, 2));
        model.redo().unwrap();
        assert!(!bold_at(&model.model, 2, 2));
        assert_invalid_settings(&model.model);
        let mut raw = model.model;
        raw.insert_rows(0, 1, 1).unwrap();
        raw.add_conditional_formatting(0, "D1", bold_rule("$A$3>3"))
            .unwrap();
        assert!(raw.evaluate_incremental(&[]).is_none());
        assert_invalid_settings(&raw);
        assert!(bold_at(&raw, 1, 4));
    }
}

#[test]
fn negative_timestamps_floor_to_the_previous_second() {
    for (ms, seconds) in [
        (-1, -1),
        (-999, -1),
        (-1000, -1),
        (-1500, -2),
        (-2000, -2),
        (0, 0),
        (1, 0),
    ] {
        let mut model = new_empty_model();
        model._set("A1", "=NOW()");
        model._set("A2", "=TODAY()");
        with_advancing_time(ms, 0, || {
            model.evaluate_headless();
            let expected = 25569.0 + seconds as f64 / 86400.0;
            assert!((number(&model, "A1") - expected).abs() < 1e-10, "{ms}");
            assert_eq!(number(&model, "A2"), expected.floor());
        });
    }
}

fn error_message(model: &Model) -> String {
    match model._get_cell("A1") {
        Cell::CellFormula {
            v: FormulaValue::Error { d, .. },
            ..
        }
        | Cell::ArrayFormula {
            v: FormulaValue::Error { d, .. },
            ..
        } => d.m,
        other => panic!("expected formula error: {other:?}"),
    }
}

#[test]
#[cfg(not(target_family = "wasm"))]
fn configured_factorial_and_distinct_recursion_diagnostics() {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            let mut model = new_empty_model();
            assert_eq!(model.get_max_lambda_depth(), 256);
            assert_eq!(model.get_max_evaluation_stack(), 256 * 1024);
            model.set_max_evaluation_stack(8 * 1024 * 1024);
            model
                .new_defined_name("FACTORIAL", None, "LAMBDA(n,IF(n=0,1,n*FACTORIAL(n-1)))")
                .unwrap();
            model._set("A1", "=FACTORIAL(70)");
            model.evaluate_headless();
            assert!(model.evaluation_report().converged);
            assert!((number(&model, "A1") / 1.197857166996989e100 - 1.0).abs() < 1e-14);
            model.set_max_lambda_depth(8);
            model.evaluate_headless();
            assert_eq!(
                model.evaluation_report().failure,
                Some(CalculationFailure::RecursionLimit)
            );
            assert_eq!(
                error_message(&model),
                "Configured LAMBDA call-depth limit (8) reached"
            );
            model.set_max_lambda_depth(256);
            model.set_max_evaluation_stack(1);
            model.evaluate_headless();
            assert_eq!(
                model.evaluation_report().failure,
                Some(CalculationFailure::RecursionLimit)
            );
            assert_eq!(
                error_message(&model),
                "Configured evaluation stack budget (1 bytes) exceeded"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn recursion_diagnostics_are_bounded_on_all_targets() {
    let mut model = new_empty_model();
    model
        .new_defined_name("FOREVER", None, "LAMBDA(x,FOREVER(x))")
        .unwrap();
    model._set("A1", "=FOREVER(1)");
    model.set_max_lambda_depth(1);
    model.evaluate_headless();
    assert_eq!(
        model.evaluation_report().failure,
        Some(CalculationFailure::RecursionLimit)
    );
    assert_eq!(
        error_message(&model),
        "Configured LAMBDA call-depth limit (1) reached"
    );
    model.set_max_lambda_depth(256);
    model.set_max_evaluation_stack(1);
    model.evaluate_headless();
    assert_eq!(
        model.evaluation_report().failure,
        Some(CalculationFailure::RecursionLimit)
    );
    assert_eq!(
        error_message(&model),
        "Configured evaluation stack budget (1 bytes) exceeded"
    );
}

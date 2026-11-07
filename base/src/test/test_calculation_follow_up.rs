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
fn reload_formats_saved_values_without_calculating_or_reporting_success() {
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
        assert_eq!(number(&restored, "A1"), first);
        assert!(!bold_at(&restored, 1, 2));
        restored.set_theme(restored.get_theme());
        restored.evaluate_conditional_formatting();
        assert_eq!(clock_reads(), 2);
        assert!(!bold_at(&restored, 1, 2));
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

fn converged_cycle_display() -> Model<'static> {
    let mut model = new_empty_model();
    model._set("A1", "=B1+1");
    model._set("B1", "=MIN(A1,5)");
    model._set("D1", "=RAND()");
    model._set("E1", "=HYPERLINK(\"https://example.com\",\"link\")");
    model
        .add_conditional_formatting(0, "C1", bold_rule("$A$1>3"))
        .unwrap();
    model.set_iterative_calculation(true, 100, 0.001).unwrap();
    assert_eq!(number(&model, "A1"), 6.0);
    assert_eq!(number(&model, "B1"), 5.0);
    model
}

#[test]
fn invalid_and_paused_structural_edits_preserve_values_and_links() {
    for paused in [false, true] {
        let mut raw = converged_cycle_display();
        let random = number(&raw, "D1");
        if !paused {
            raw.workbook
                .settings
                .iterative_calculation
                .maximum_iterations = 0;
        }
        let mut model = UserModel::from_model(raw);
        if paused {
            model.pause_evaluation();
        }
        model.insert_rows(0, 1, 1).unwrap();
        assert_eq!(number(&model.model, "A2"), 6.0);
        assert_eq!(number(&model.model, "B2"), 5.0);
        assert_eq!(number(&model.model, "D2"), random);
        assert!(bold_at(&model.model, 2, 3));
        assert_eq!(model.model.get_links_list(0).unwrap()[0].row, 2);
        if !paused {
            assert_invalid_settings(&model.model);
        }
        let saved: crate::types::Workbook = bitcode::decode(&model.to_bytes()).unwrap();
        assert!(matches!(
            saved.worksheets[0].cell(2, 1).as_deref(),
            Some(Cell::CellFormula {
                v: FormulaValue::Number(6.0),
                ..
            })
        ));
        model.undo().unwrap();
        assert_eq!(number(&model.model, "A1"), 6.0);
        assert_eq!(number(&model.model, "D1"), random);
        assert_clock_link(&model.model, 0, 1, 5);
        model.redo().unwrap();
        assert_eq!(number(&model.model, "A2"), 6.0);
        model.rename_sheet(0, "Renamed").unwrap();
        assert_eq!(
            model.model.get_cell_value_by_index(0, 2, 1).unwrap(),
            CellValue::Number(6.0)
        );
        assert_eq!(model.model.get_links_list(0).unwrap().len(), 1);
        model
            .add_conditional_formatting(0, "C3", bold_rule("$A$2>3"))
            .unwrap();
        model.undo().unwrap();
        model.redo().unwrap();
        assert_eq!(
            model.model.get_cell_value_by_index(0, 2, 1).unwrap(),
            CellValue::Number(6.0)
        );
        assert!(bold_at(&model.model, 3, 3));
    }
}

fn assert_clock_link(model: &Model, sheet: u32, row: i32, column: i32) {
    let links = model.get_links_list(sheet).unwrap();
    assert!(links
        .iter()
        .any(|link| link.dynamic && link.row == row && link.column == column));
}

#[test]
fn formatting_reads_are_side_effect_free_through_all_reference_forms() {
    let mut model = converged_cycle_display();
    model
        .new_defined_name("STORED", None, "Sheet1!$A$1")
        .unwrap();
    model
        .add_conditional_formatting(
            0,
            "G1",
            bold_rule("AND(INDIRECT(\"A1\")=6,SUM(A1:B1)=11,STORED=6,D1>=0)"),
        )
        .unwrap();
    model
        .add_conditional_formatting(
            0,
            "G2",
            bold_rule("HYPERLINK(\"https://other.example\",TRUE())"),
        )
        .unwrap();
    let cells = model.evaluation.cells.clone();
    let report = model.evaluation_report().clone();
    let links = model.get_links_list(0).unwrap();
    let bytes = model.to_bytes();
    let random = number(&model, "D1");
    model.evaluation.cells.clear();
    let empty_cells = model.evaluation.cells.clone();
    model.evaluate_conditional_formatting();
    assert_eq!(model.to_bytes(), bytes);
    assert_eq!(model.get_links_list(0).unwrap(), links);
    assert_eq!(model.evaluation_report(), &report);
    assert_eq!(model.evaluation.cells, empty_cells);
    assert_eq!(number(&model, "D1"), random);
    assert!(bold_at(&model, 1, 7));
    assert!(bold_at(&model, 2, 7));
    model.evaluation.cells = cells;
}

#[test]
fn intact_spills_survive_edits_and_partial_spills_wait_for_calculation() {
    let mut model = new_empty_model();
    model._set("A1", "=SEQUENCE(5)");
    model
        .add_conditional_formatting(0, "B1", bold_rule("SUM(A1#)=15"))
        .unwrap();
    model.evaluate();
    model.workbook.settings.iterative_calculation.enabled = true;
    model
        .workbook
        .settings
        .iterative_calculation
        .maximum_iterations = 0;
    model.insert_rows(0, 10, 1).unwrap();
    model.evaluate();
    assert_eq!(number(&model, "A3"), 3.0);
    assert!(bold_at(&model, 1, 2));
    model.insert_rows(0, 1, 1).unwrap();
    assert_eq!(number(&model, "A4"), 3.0);
    assert!(bold_at(&model, 2, 2));
    model.move_columns_action(0, 1, 1, 2).unwrap();
    assert_eq!(number(&model, "C4"), 3.0);
    model.insert_rows(0, 4, 1).unwrap();
    assert_eq!(number(&model, "C2"), 1.0);
    assert_eq!(
        model.get_cell_value_by_index(0, 3, 3).unwrap(),
        CellValue::None
    );
    assert_eq!(
        model.get_cell_value_by_index(0, 5, 3).unwrap(),
        CellValue::None
    );
    model.workbook.settings.iterative_calculation.enabled = false;
    model.evaluate();
    assert_eq!(number(&model, "C4"), 3.0);
    assert!(model.evaluation_report().converged);
}

#[test]
fn local_history_restores_deleted_spills_and_dynamic_links_without_calculating() {
    let mut raw = new_empty_model();
    raw._set("A1", "=SEQUENCE(5)");
    raw._set("B1", "=HYPERLINK(\"https://example.com\",\"link\")");
    raw.evaluate();
    let mut model = UserModel::from_model(raw);
    model.pause_evaluation();
    let evaluations = model.model.evaluation.full_evaluations;
    model.delete_rows(0, 1, 1).unwrap();
    assert!(model.model.get_links_list(0).unwrap().is_empty());
    model.undo().unwrap();
    assert_eq!(number(&model.model, "A3"), 3.0);
    assert_clock_link(&model.model, 0, 1, 2);
    model.redo().unwrap();
    assert!(model.model.get_links_list(0).unwrap().is_empty());
    model.undo().unwrap();
    assert_eq!(number(&model.model, "A5"), 5.0);
    assert_eq!(model.model.evaluation.full_evaluations, evaluations);
}

#[test]
fn context_setters_reject_atomically_and_noops_do_not_calculate() {
    with_advancing_time(1679356799000, 1000, || {
        let mut raw = clock_model();
        raw.evaluate();
        raw.workbook.settings.iterative_calculation.enabled = true;
        raw.workbook
            .settings
            .iterative_calculation
            .maximum_iterations = 0;
        raw.evaluate();
        let bytes = raw.to_bytes();
        let report = raw.evaluation_report().clone();
        let reads = clock_reads();
        assert!(raw.set_timezone("Asia/Tokyo").is_err());
        assert!(raw.set_locale("de").is_err());
        raw.set_timezone("UTC").unwrap();
        raw.set_locale("en").unwrap();
        assert_eq!(raw.to_bytes(), bytes);
        assert_eq!(raw.evaluation_report(), &report);
        assert_eq!(clock_reads(), reads);
        assert_clock_format_matches(&raw);
        let mut model = UserModel::from_model(raw);
        let queue = model.flush_send_queue();
        assert!(model.set_timezone("Asia/Tokyo").is_err());
        assert!(model.set_locale("de").is_err());
        assert!(!model.can_undo());
        assert_eq!(model.flush_send_queue(), queue);
        model.model.workbook.settings.iterative_calculation.enabled = false;
        model.pause_evaluation();
        assert!(model.set_timezone("Asia/Tokyo").is_err());
        assert!(model.set_locale("de").is_err());
        assert_eq!(clock_reads(), reads);
        model.resume_evaluation();
        model.set_timezone("Asia/Tokyo").unwrap();
        assert_eq!(clock_reads(), reads + 1);
        model.pause_evaluation();
        let bytes = model.to_bytes();
        assert!(model.undo().is_err());
        assert_eq!(model.to_bytes(), bytes);
        assert!(model.can_undo());
        assert!(!model.can_redo());
        model.resume_evaluation();
        model.undo().unwrap();
        assert_eq!(model.get_timezone(), "UTC");
    });
}

#[test]
fn cut_and_sheet_operations_retain_the_display_and_history() {
    let mut raw = converged_cycle_display();
    raw.new_sheet();
    let mut model = UserModel::from_model(raw);
    model.pause_evaluation();
    model.set_selected_range(1, 1, 1, 5).unwrap();
    let clipboard = model.copy_to_clipboard().unwrap();
    let random = number(&model.model, "D1");
    model.set_selected_cell(3, 1).unwrap();
    model
        .paste_from_clipboard(0, clipboard.range, &clipboard.data, true)
        .unwrap();
    assert_eq!(number(&model.model, "A3"), 6.0);
    assert_eq!(number(&model.model, "D3"), random);
    assert_clock_link(&model.model, 0, 3, 5);
    model.undo().unwrap();
    assert_eq!(number(&model.model, "A1"), 6.0);
    assert_clock_link(&model.model, 0, 1, 5);
    model.move_sheet(0, 1).unwrap();
    assert_clock_link(&model.model, 1, 1, 5);
    model.delete_sheet(1).unwrap();
    assert!(model.model.get_links_list(0).unwrap().is_empty());
    model.undo().unwrap();
    assert_clock_link(&model.model, 1, 1, 5);
    assert_eq!(
        model.model.get_cell_value_by_index(1, 1, 1).unwrap(),
        CellValue::Number(6.0)
    );
}

#[test]
fn loaded_cycles_keep_saved_values_and_empty_results_wait_for_calculation() {
    let mut raw = converged_cycle_display();
    raw.workbook
        .settings
        .iterative_calculation
        .maximum_iterations = 0;
    let bytes = raw.to_bytes();
    for mut loaded in [
        Model::from_bytes(&bytes, "en").unwrap(),
        Model::from_workbook(raw.workbook.clone(), "en").unwrap(),
    ] {
        assert_eq!(number(&loaded, "A1"), 6.0);
        assert_eq!(loaded.to_bytes(), bytes);
        assert!(bold_at(&loaded, 1, 3));
        assert_eq!(
            loaded.evaluation_report().failure,
            Some(CalculationFailure::NotEvaluated)
        );
        assert!(loaded.get_links_list(0).unwrap().is_empty());
        assert!(loaded.evaluate_incremental(&[]).is_none());
        assert_invalid_settings(&loaded);
        assert_eq!(loaded.to_bytes(), bytes);
        assert_eq!(number(&loaded, "A1"), 6.0);
    }
    let mut raw = new_empty_model();
    raw._set("A1", "=1+1");
    raw.add_conditional_formatting(0, "B1", bold_rule("A1=2"))
        .unwrap();
    let bytes = raw.to_bytes();
    let mut loaded = Model::from_bytes(&bytes, "en").unwrap();
    assert_eq!(loaded.to_bytes(), bytes);
    assert!(!bold_at(&loaded, 1, 2));
    loaded.evaluate();
    assert!(bold_at(&loaded, 1, 2));
}

#[test]
fn external_context_batches_are_preflighted_in_order_without_side_effects() {
    use crate::user_model::history::{Diff, DiffType, QueueDiffs};
    let mut raw = clock_model();
    raw.evaluate();
    raw.workbook.settings.iterative_calculation.enabled = true;
    raw.workbook
        .settings
        .iterative_calculation
        .maximum_iterations = 0;
    raw.evaluate();
    let mut model = UserModel::from_model(raw);
    let bytes = model.to_bytes();
    let report = model.model.evaluation_report().clone();
    let change = Diff::SetTimezone {
        old_value: "UTC".into(),
        new_value: "Asia/Tokyo".into(),
    };
    let invalid = model.get_iterative_calculation();
    let mut valid = invalid.clone();
    valid.maximum_iterations = 100;
    let repair = Diff::SetIterativeCalculation {
        old_value: invalid,
        new_value: valid,
    };
    let send = |list| {
        bitcode::encode(&vec![QueueDiffs {
            r#type: DiffType::Redo,
            list,
        }])
    };
    assert!(model
        .apply_external_diffs(&send(vec![change.clone(), repair.clone()]))
        .is_err());
    assert_eq!(model.to_bytes(), bytes);
    assert_eq!(model.model.evaluation_report(), &report);
    assert_clock_format_matches(&model.model);
    assert!(!model.can_undo());
    model
        .apply_external_diffs(&send(vec![repair, change]))
        .unwrap();
    assert_eq!(model.get_timezone(), "Asia/Tokyo");
    assert!(model.model.evaluation_report().converged);
    let mut peer = UserModel::new_empty("peer", "en", "UTC", "en").unwrap();
    peer.set_timezone("Asia/Tokyo").unwrap();
    let queue = peer.flush_send_queue();
    let mut paused = UserModel::new_empty("paused", "en", "UTC", "en").unwrap();
    paused.pause_evaluation();
    let before = paused.to_bytes();
    assert!(paused.apply_external_diffs(&queue).is_err());
    assert_eq!(paused.to_bytes(), before);
}

#[test]
fn edit_batches_refresh_formatting_once_without_reading_a_new_display_clock() {
    with_advancing_time(1679356799000, 86_400_000, || {
        let mut raw = clock_model();
        raw.evaluate();
        let mut model = UserModel::from_model(raw);
        model.pause_evaluation();
        let reads = clock_reads();
        let evaluations = model.model.evaluation.full_evaluations;
        let formats = model.model.evaluation.cf_evaluations;
        model.insert_rows(0, 1, 1).unwrap();
        assert_eq!(model.model.evaluation.cf_evaluations, formats + 1);
        assert_eq!(model.model.evaluation.full_evaluations, evaluations);
        assert!(bold_at(&model.model, 2, 2));
        model.undo().unwrap();
        assert_clock_format_matches(&model.model);
        assert_eq!(model.model.evaluation.cf_evaluations, formats + 2);
        assert_eq!(clock_reads(), reads);
        model.resume_evaluation();
        model.insert_columns(0, 1, 1).unwrap();
        assert_eq!(model.model.evaluation.cf_evaluations, formats + 3);
        assert_eq!(model.model.evaluation.full_evaluations, evaluations + 1);
        assert_eq!(clock_reads(), reads + 1);
        assert!(bold_at(&model.model, 1, 3));
    });
}

#[test]
fn nested_formatting_scopes_use_active_time_without_publishing_incomplete_work() {
    let mut model = clock_model();
    with_advancing_time(1679356799000, 86_400_000, || {
        model.evaluate();
        let display = model.evaluation.display_timestamp;
        let clock = model.start_calculation_clock();
        model.evaluate_conditional_formatting();
        assert!(!bold_at(&model, 1, 2));
        assert_eq!(model.evaluation.display_timestamp, display);
        assert_eq!(clock_reads(), 2);
        drop(clock);
        model.evaluate_conditional_formatting();
        assert_clock_format_matches(&model);
        assert_eq!(clock_reads(), 2);
        assert!(model.evaluation.calculation_time.lock().unwrap().is_none());
    });
}

#[test]
fn columns_and_partial_deletions_preserve_cached_results_and_manual_links() {
    let mut raw = converged_cycle_display();
    raw.set_cell_link(
        0,
        1,
        6,
        crate::types::Link::External {
            target: "https://manual.example".into(),
            tooltip: None,
        },
    )
    .unwrap();
    raw.workbook
        .settings
        .iterative_calculation
        .maximum_iterations = 0;
    let mut model = UserModel::from_model(raw);
    model.insert_columns(0, 1, 1).unwrap();
    assert_eq!(number(&model.model, "B1"), 6.0);
    assert_clock_link(&model.model, 0, 1, 6);
    assert!(model.model.get_cell_link(0, 1, 7).unwrap().is_some());
    model.delete_columns(0, 6, 2).unwrap();
    assert!(model.model.get_links_list(0).unwrap().is_empty());
    model.undo().unwrap();
    assert_clock_link(&model.model, 0, 1, 6);
    assert!(model.model.get_cell_link(0, 1, 7).unwrap().is_some());
    assert_invalid_settings(&model.model);
    let mut raw = new_empty_model();
    raw._set("A1", "=SEQUENCE(5)");
    raw.evaluate();
    raw.delete_rows(0, 3, 1).unwrap();
    assert_eq!(number(&raw, "A1"), 1.0);
    assert_eq!(
        raw.get_cell_value_by_index(0, 2, 1).unwrap(),
        CellValue::None
    );
    raw.evaluate();
    assert_eq!(number(&raw, "A3"), 3.0);
}

#[test]
fn explicit_formula_replacement_retires_links_and_local_undo_restores_them() {
    let mut raw = new_empty_model();
    raw._set("A1", "=HYPERLINK(\"https://example.com\",\"link\")");
    raw.evaluate();
    let mut model = UserModel::from_model(raw);
    model.pause_evaluation();
    model.set_user_input(0, 1, 1, "=1+1").unwrap();
    assert!(model.model.get_links_list(0).unwrap().is_empty());
    assert!(matches!(
        model.model._get_cell("A1"),
        Cell::CellFormula {
            v: FormulaValue::Unevaluated,
            ..
        }
    ));
    model.undo().unwrap();
    assert_clock_link(&model.model, 0, 1, 1);
    model.redo().unwrap();
    assert!(model.model.get_links_list(0).unwrap().is_empty());
    model.undo().unwrap();
    model.model.update_cell_with_number(0, 1, 1, 2.0).unwrap();
    assert!(model.model.get_links_list(0).unwrap().is_empty());
}

#[test]
fn partial_spill_cut_and_replacement_leave_no_orphaned_children() {
    for cut in [false, true] {
        let mut raw = new_empty_model();
        raw._set("A1", "=SEQUENCE(5)");
        raw.evaluate();
        let mut model = UserModel::from_model(raw);
        model.pause_evaluation();
        if cut {
            model.set_selected_cell(3, 1).unwrap();
            model.set_selected_range(3, 1, 3, 1).unwrap();
            let clip = model.copy_to_clipboard().unwrap();
            model.set_selected_cell(3, 3).unwrap();
            model
                .paste_from_clipboard(0, clip.range, &clip.data, true)
                .unwrap();
        } else {
            model.set_user_input(0, 3, 1, "9").unwrap();
        }
        assert_eq!(number(&model.model, "A1"), 1.0);
        assert_eq!(
            model.model.get_cell_value_by_index(0, 2, 1).unwrap(),
            CellValue::None
        );
        assert_eq!(
            model.model.get_cell_value_by_index(0, 5, 1).unwrap(),
            CellValue::None
        );
        assert_eq!(
            model.model.evaluation_report().failure,
            Some(CalculationFailure::NotEvaluated)
        );
        model.undo().unwrap();
        assert_eq!(number(&model.model, "A3"), 3.0);
    }
}

#[test]
fn whole_spill_cut_preserves_anchor_kind_extent_values_and_spill_references() {
    let mut raw = new_empty_model();
    raw._set("A1", "=SEQUENCE(5)");
    raw.add_conditional_formatting(0, "B1", bold_rule("SUM(A1#)=15"))
        .unwrap();
    raw.evaluate();
    let mut model = UserModel::from_model(raw);
    model.pause_evaluation();
    model.set_selected_range(1, 1, 5, 1).unwrap();
    let clip = model.copy_to_clipboard().unwrap();
    model.set_selected_cell(2, 3).unwrap();
    model
        .paste_from_clipboard(0, clip.range, &clip.data, true)
        .unwrap();
    assert_eq!(number(&model.model, "C4"), 3.0);
    assert!(matches!(
        model.model._get_cell("C2"),
        Cell::ArrayFormula {
            r: (1, 5),
            kind: crate::types::ArrayKind::Dynamic,
            ..
        }
    ));
    model
        .add_conditional_formatting(0, "D2", bold_rule("SUM(C2#)=15"))
        .unwrap();
    assert!(bold_at(&model.model, 2, 4));
    model.undo().unwrap();
    model.undo().unwrap();
    assert_eq!(number(&model.model, "A3"), 3.0);
    assert!(bold_at(&model.model, 1, 2));
}

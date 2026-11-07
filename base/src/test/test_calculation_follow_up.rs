#![allow(clippy::unwrap_used, clippy::panic)]

use crate::{
    cell::CellValue,
    mock_time::{clock_reads, with_advancing_time},
    test::util::new_empty_model,
    types::{CalculationFailure, Cell, FormulaValue},
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
    use crate::cf_types::CfRuleInput;
    use crate::types::Dxf;
    let mut model = new_empty_model();
    model._set("A1", "=NOW()");
    model._set("B1", "1");
    model
        .add_conditional_formatting(
            0,
            "B1",
            CfRuleInput::Formula {
                formula: "NOW()=$A$1".into(),
                format: Dxf::default(),
                stop_if_true: false,
            },
        )
        .unwrap();
    with_advancing_time(1679356799000, 86_400_000, || {
        model.evaluate();
        assert_eq!(clock_reads(), 1);
        assert!(!model.cf_cache.get(&(0, 1, 2)).unwrap().is_empty());
        assert!(model.evaluate_incremental(&[]).is_some());
        assert_eq!(clock_reads(), 2);
        assert!(!model.cf_cache.get(&(0, 1, 2)).unwrap().is_empty());
    });
}

#[test]
fn clock_is_released_on_early_return_and_panic() {
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
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _clock = model.start_calculation_clock();
            panic!("test clock cleanup");
        }))
        .is_err());
        assert_eq!(clock_reads(), 2);
        model.workbook.settings.iterative_calculation.enabled = false;
        model.evaluate_headless();
        assert_eq!(clock_reads(), 3);
    });
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

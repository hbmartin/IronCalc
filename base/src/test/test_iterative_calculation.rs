#![allow(clippy::unwrap_used, clippy::panic)]

use crate::cell::CellValue;
use crate::test::util::new_empty_model;
use crate::types::IterativeCalculation;
use crate::Model;

fn number(model: &Model, cell: &str) -> f64 {
    match model
        .get_cell_value_by_ref(&format!("Sheet1!{cell}"))
        .unwrap()
    {
        CellValue::Number(n) => n,
        other => panic!("expected a number in {cell}, got {other:?}"),
    }
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-6,
        "expected {expected}, got {actual}"
    );
}

#[test]
fn defaults() {
    let model = new_empty_model();
    assert_eq!(
        model.get_iterative_calculation(),
        IterativeCalculation {
            enabled: false,
            maximum_iterations: 100,
            maximum_change: 0.001,
        }
    );
}

#[test]
fn invalid_settings_are_rejected_without_mutating_settings() {
    let mut model = new_empty_model();
    for (iterations, delta) in [
        (0, 0.001),
        (32768, 0.001),
        (100, -0.1),
        (100, f64::NAN),
        (100, f64::INFINITY),
    ] {
        assert!(model
            .set_iterative_calculation(true, iterations, delta)
            .is_err());
        assert_eq!(
            model.get_iterative_calculation(),
            IterativeCalculation::default()
        );
    }
}

#[test]
fn reports_convergence_and_iteration_exhaustion() {
    use crate::types::CalculationFailure;
    let mut model = new_empty_model();
    model._set("A1", "=A1*0.5+10");
    model.set_iterative_calculation(true, 100, 1e-9).unwrap();
    let report = model.evaluation_report();
    assert!(report.converged && report.had_circular_references);
    assert!(report.iterations > 1 && report.residual < 1e-9);
    model._set("A1", "=A1+1");
    model.set_iterative_calculation(true, 10, 1e-9).unwrap();
    assert_eq!(
        model.evaluation_report().failure,
        Some(CalculationFailure::IterationLimit)
    );
    assert!(!model.evaluation_report().converged);
    assert_eq!(model.evaluation_report().iterations, 10);
    assert_eq!(model.evaluation_report().residual, 1.0);
}

#[test]
fn oscillating_and_nonnumeric_cycles_are_not_successful() {
    use crate::types::CalculationFailure;
    let mut model = new_empty_model();
    model._set("A1", "=1-A1");
    model.set_iterative_calculation(true, 20, 1e-9).unwrap();
    assert_eq!(
        model.evaluation_report().failure,
        Some(CalculationFailure::IterationLimit)
    );
    model._set("A1", "=IF(A1=0,\"text\",\"text\")");
    model.evaluate();
    assert_eq!(
        model.evaluation_report().failure,
        Some(CalculationFailure::NonNumericCycle)
    );
}

#[test]
fn inactive_circular_branch_is_not_a_runtime_cycle() {
    let mut model = new_empty_model();
    model._set("A1", "=IF(FALSE,A1,42)");
    model.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert_eq!(number(&model, "A1"), 42.0);
    assert!(model.evaluation_report().converged);
    assert!(!model.evaluation_report().had_circular_references);
    assert_eq!(model.evaluation_report().iterations, 1);
}

#[test]
fn set_and_get() {
    let mut model = new_empty_model();
    model.set_iterative_calculation(true, 30, 0.5).unwrap();
    assert_eq!(
        model.get_iterative_calculation(),
        IterativeCalculation {
            enabled: true,
            maximum_iterations: 30,
            maximum_change: 0.5,
        }
    );
}

#[test]
fn circular_reference_is_an_error_when_disabled() {
    let mut model = new_empty_model();
    model._set("A1", "=B1/2+10");
    model._set("B1", "=A1+5");
    model.evaluate();
    assert_eq!(model._get_text("A1"), "#CIRC!");
    assert_eq!(model._get_text("B1"), "#CIRC!");
}

#[test]
fn disabling_brings_back_the_error() {
    let mut model = new_empty_model();
    model._set("A1", "=B1/2+10");
    model._set("B1", "=A1+5");
    model.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert_close(number(&model, "A1"), 25.0);
    model.set_iterative_calculation(false, 100, 1e-9).unwrap();
    assert_eq!(model._get_text("A1"), "#CIRC!");
    assert_eq!(model._get_text("B1"), "#CIRC!");
}

#[test]
fn two_cell_cycle_settles() {
    let mut model = new_empty_model();
    model._set("A1", "=B1/2+10");
    model._set("B1", "=A1+5");
    model.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert_close(number(&model, "A1"), 25.0);
    assert_close(number(&model, "B1"), 30.0);
}

#[test]
fn self_reference_starts_from_zero() {
    let mut model = new_empty_model();
    model._set("A1", "=A1*0.5+10");
    model.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert_close(number(&model, "A1"), 20.0);
}

#[test]
fn cells_reading_a_cycle_see_the_settled_values() {
    let mut model = new_empty_model();
    model._set("A1", "=B1/2+10");
    model._set("B1", "=A1+5");
    model._set("C1", "=A1*2");
    model._set("D1", "=C1+B1");
    model.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert_close(number(&model, "C1"), 50.0);
    assert_close(number(&model, "D1"), 80.0);
}

#[test]
fn cycle_across_sheets() {
    let mut model = new_empty_model();
    model.add_sheet("Other").unwrap();
    model._set("Sheet1!A1", "=Other!A1/2+10");
    model._set("Other!A1", "=Sheet1!A1+5");
    model.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert_close(number(&model, "A1"), 25.0);
    assert_close(
        match model.get_cell_value_by_ref("Other!A1").unwrap() {
            CellValue::Number(n) => n,
            other => panic!("expected a number, got {other:?}"),
        },
        30.0,
    );
}

#[test]
fn growing_reference_stops_at_the_maximum_iterations() {
    let mut model = new_empty_model();
    model._set("A1", "=A1+1");
    model.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert_close(number(&model, "A1"), 100.0);
    model.set_iterative_calculation(true, 7, 1e-9).unwrap();
    assert_close(number(&model, "A1"), 7.0);
}

#[test]
fn evaluating_again_gives_the_same_values() {
    let mut model = new_empty_model();
    model._set("A1", "=A1+1");
    model.set_iterative_calculation(true, 12, 1e-9).unwrap();
    model.evaluate();
    model.evaluate();
    assert_close(number(&model, "A1"), 12.0);
}

#[test]
fn stops_once_the_change_is_below_the_maximum_change() {
    let mut model = new_empty_model();
    model._set("A1", "=A1*0.5+10");
    model.set_iterative_calculation(true, 100, 1.0).unwrap();
    let value = number(&model, "A1");
    assert!(value < 20.0 && value > 18.0, "got {value}");
    model.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert_close(number(&model, "A1"), 20.0);
}

#[test]
fn a_workbook_without_cycles_is_not_affected() {
    let mut model = new_empty_model();
    model._set("A1", "=5");
    model._set("B1", "=A1+1");
    model._set("C1", "=B1*2");
    model.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert_close(number(&model, "A1"), 5.0);
    assert_close(number(&model, "B1"), 6.0);
    assert_close(number(&model, "C1"), 12.0);
}

#[test]
fn a_workbook_without_cycles_is_evaluated_once() {
    let mut model = new_empty_model();
    model._set("A1", "=RAND()");
    model._set("B1", "=A1");
    model.set_iterative_calculation(false, 100, 1e-9).unwrap();
    let without = number(&model, "B1");
    assert!((0.0..1.0).contains(&without));
    model.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert_close(number(&model, "B1"), number(&model, "A1"));
    assert_eq!(model.evaluation_report().iterations, 1);
}

#[test]
fn settled_values_do_not_depend_on_cell_positions() {
    let mut forward = new_empty_model();
    forward._set("A1", "=B1/2+10");
    forward._set("B1", "=C1+5");
    forward._set("C1", "=A1*0.25+B1*0.25");
    forward._set("D1", "=C1+A1");
    forward.set_iterative_calculation(true, 200, 1e-12).unwrap();
    let mut backward = new_empty_model();
    backward._set("D1", "=C1/2+10");
    backward._set("C1", "=B1+5");
    backward._set("B1", "=D1*0.25+C1*0.25");
    backward._set("A1", "=B1+D1");
    backward
        .set_iterative_calculation(true, 200, 1e-12)
        .unwrap();
    assert!(forward.evaluation_report().converged && backward.evaluation_report().converged);
    for (left, right) in [("A1", "D1"), ("B1", "C1"), ("C1", "B1"), ("D1", "A1")] {
        assert_close(number(&forward, left), number(&backward, right));
    }
}

#[test]
fn a_cycle_that_started_as_an_error_recovers_when_enabled() {
    let mut model = new_empty_model();
    model._set("A1", "=A1/2+4");
    model.evaluate();
    assert_eq!(model._get_text("A1"), "#CIRC!");
    model.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert_close(number(&model, "A1"), 8.0);
}

#[test]
fn a_cell_outside_the_cycle_can_be_edited() {
    let mut model = new_empty_model();
    model._set("A1", "=B1/2+C1");
    model._set("B1", "=A1+5");
    model._set("C1", "10");
    model.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert_close(number(&model, "A1"), 25.0);
    model._set("C1", "20");
    model.evaluate();
    assert_close(number(&model, "A1"), 45.0);
    assert_close(number(&model, "B1"), 50.0);
}

#[test]
fn nonterminating_lambda_returns_a_bounded_diagnostic() {
    use crate::types::CalculationFailure;
    let mut model = new_empty_model();
    model
        .new_defined_name("FACTORIAL", None, "LAMBDA(x,IF(x=1,1,x*FACTORIAL(x-1)))")
        .unwrap();
    model._set("A1", "=FACTORIAL(2.5)");
    model.evaluate_headless();
    assert_eq!(model._get_text("A1"), "#NUM!");
    assert_eq!(
        model.evaluation_report().failure,
        Some(CalculationFailure::RecursionLimit)
    );
    model._set("A1", "=FACTORIAL(5)");
    model.evaluate_headless();
    assert_eq!(number(&model, "A1"), 120.0);
    assert!(model.evaluation_report().converged);
}

#[test]
fn stochastic_values_cannot_create_false_convergence() {
    use crate::types::CalculationFailure;
    let mut model = new_empty_model();
    model._set("B1", "=RAND()");
    model._set("A1", "=A1/2+B1");
    model.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert_eq!(
        model.evaluation_report().failure,
        Some(CalculationFailure::VolatileCycle)
    );
    assert!(!model.evaluation_report().converged);
}

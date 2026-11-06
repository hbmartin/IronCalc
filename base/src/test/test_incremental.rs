#![allow(clippy::unwrap_used)]
use crate::test::util::new_empty_model;
use crate::types::CalculationFailure;
use crate::Model;

fn assert_same(left: &Model, right: &Model) {
    for cell in left.get_all_cells() {
        assert_eq!(
            left.get_cell_value_by_index(cell.index, cell.row, cell.column)
                .unwrap(),
            right
                .get_cell_value_by_index(cell.index, cell.row, cell.column)
                .unwrap(),
            "cell ({},{},{})",
            cell.index,
            cell.row,
            cell.column
        );
    }
}

#[test]
fn randomized_edits_and_dynamic_targets_match_full_evaluation() {
    let mut incremental = new_empty_model();
    let mut full = new_empty_model();
    for model in [&mut incremental, &mut full] {
        model._set("A1", "1");
        model._set("A2", "10");
        model._set("A3", "20");
        model._set("B1", "=A2+A3");
        model._set("B2", "=IF(A1=1,A2,A3)");
        model._set("C1", "=B1*B2");
        model._set("C2", "=SUM(A1:A3)");
        model._set("D1", "=OFFSET(A1,A1,0)");
        model._set("D2", "=D1+C1");
        model.evaluate_headless();
    }
    let mut state = 123_u64;
    for i in 0..200 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let row = 1 + (i % 3);
        let value = if row == 1 {
            1 + state % 2
        } else {
            state % 1000
        };
        for model in [&mut incremental, &mut full] {
            model
                .update_cell_with_number(0, row, 1, value as f64)
                .unwrap();
        }
        assert!(incremental
            .evaluate_incremental_headless(&[(0, row, 1)])
            .is_some());
        full.evaluate_headless();
        assert_same(&incremental, &full);
    }
}

#[test]
fn conservative_range_cycle_is_not_a_runtime_cycle() {
    let mut model = new_empty_model();
    model._set("A1", "1");
    model._set("B1", "2");
    model._set("A2", "10");
    model._set("B2", "=HLOOKUP(A3,A1:B2,2,FALSE)");
    model._set("A3", "1");
    model.evaluate_headless();
    assert_eq!(model._get_text("B2"), "10");
    model._set("A2", "15");
    assert!(model.evaluate_incremental_headless(&[(0, 2, 1)]).is_some());
    assert_eq!(model._get_text("B2"), "15");
    model._set("A3", "2");
    assert!(model.evaluate_incremental_headless(&[(0, 3, 1)]).is_none());
    assert_eq!(
        model.evaluation_report().failure,
        Some(CalculationFailure::CircularReference)
    );
    model._set("A3", "1");
    model.evaluate_incremental_headless(&[(0, 3, 1)]);
    assert_eq!(model._get_text("B2"), "15");
    assert!(model.evaluation_report().converged);
}

#[test]
fn formula_replacement_and_new_formulas_rebuild_dependencies() {
    let mut model = new_empty_model();
    model._set("A1", "10");
    model._set("B1", "=A1*2");
    model._set("C1", "=B1+1");
    model.evaluate_headless();
    model._set("B1", "3");
    assert!(model.evaluate_incremental_headless(&[(0, 1, 2)]).is_some());
    assert_eq!(model._get_text("C1"), "4");
    model._set("B1", "=A1*3");
    assert!(model.evaluate_incremental_headless(&[(0, 1, 2)]).is_some());
    assert_eq!(model._get_text("C1"), "31");
    model._set("A1", "20");
    assert!(model.evaluate_incremental_headless(&[(0, 1, 1)]).is_some());
    assert_eq!(model._get_text("C1"), "61");
}

#[test]
fn iterative_fallback_resets_cycle_initialization() {
    let mut model = new_empty_model();
    model._set("B1", "10");
    model._set("A1", "=A1/2+B1");
    model.set_iterative_calculation(true, 100, 1e-9).unwrap();
    for n in [20, 5, 20] {
        model._set("B1", &n.to_string());
        assert!(model.evaluate_incremental_headless(&[(0, 1, 2)]).is_none());
        assert!(model.evaluation_report().converged);
        assert!(
            (match model.get_cell_value_by_ref("Sheet1!A1").unwrap() {
                crate::cell::CellValue::Number(value) => value,
                _ => 0.0,
            } - n as f64 * 2.0)
                .abs()
                < 1e-8
        );
    }
}

#[test]
fn sessions_share_parsed_trees_but_isolate_values_and_scratch() {
    let mut source = new_empty_model();
    source._set("A1", "10");
    source._set("B1", "=A1*2");
    source.evaluate_headless();
    source.evaluate_incremental_headless(&[]);
    let mut other = source.fork_evaluation_session().unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &source.parsed_formulas[0][0].0,
        &other.parsed_formulas[0][0].0
    ));
    other._set("A1", "20");
    other.evaluate_incremental_headless(&[(0, 1, 1)]);
    assert_eq!(other._get_text("B1"), "40");
    assert_eq!(source._get_text("B1"), "20");
}

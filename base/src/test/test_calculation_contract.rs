#![allow(clippy::unwrap_used, clippy::panic)]

use crate::{cell::CellValue, test::util::new_empty_model, types::CalculationFailure, Model};

fn number(model: &Model, cell: &str) -> f64 {
    match model
        .get_cell_value_by_ref(&format!("Sheet1!{cell}"))
        .unwrap()
    {
        CellValue::Number(n) => n,
        value => panic!("{cell}: {value:?}"),
    }
}
#[cfg(not(target_family = "wasm"))]
fn sumto(model: &mut Model) {
    model
        .new_defined_name("SUMTO", None, "LAMBDA(n,IF(n=0,0,n+SUMTO(n-1)))")
        .unwrap();
}

#[test]
fn new_and_loaded_models_require_evaluation_before_convergence() {
    let mut model = new_empty_model();
    assert!(!model.evaluation_report().converged);
    assert_eq!(model.evaluation_report().iterations, 0);
    assert_eq!(
        model.evaluation_report().failure,
        Some(CalculationFailure::NotEvaluated)
    );

    model._set("A1", "=21*2");
    model.evaluate_headless();
    assert!(model.evaluation_report().converged);
    assert_eq!(number(&model, "A1"), 42.0);

    let bytes = model.to_bytes();
    let mut restored = Model::from_bytes(&bytes, "en").unwrap();
    assert!(!restored.evaluation_report().converged);
    assert_eq!(restored.evaluation_report().iterations, 0);
    assert_eq!(
        restored.evaluation_report().failure,
        Some(CalculationFailure::NotEvaluated)
    );

    restored.evaluate_headless();
    assert!(restored.evaluation_report().converged);
    assert_eq!(restored.evaluation_report().iterations, 1);
    assert_eq!(number(&restored, "A1"), 42.0);
}

#[test]
fn invalidated_models_require_reevaluation_before_convergence() {
    let mut model = new_empty_model();
    model._set("A1", "=21*2");
    model.evaluate_headless();
    assert!(model.evaluation_report().converged);

    model.insert_rows(0, 1, 1).unwrap();
    assert!(!model.evaluation_report().converged);
    assert_eq!(model.evaluation_report().iterations, 0);
    assert_eq!(
        model.evaluation_report().failure,
        Some(CalculationFailure::NotEvaluated)
    );

    model.evaluate_headless();
    assert!(model.evaluation_report().converged);
    assert_eq!(model.evaluation_report().iterations, 1);
    assert_eq!(number(&model, "A2"), 42.0);
}

#[test]
fn scalar_dynamic_cycles_converge() {
    for formula in [
        "=A1*Rate+10",
        "=A1*INDEX(C1:C2,1)+10",
        "=LET(r,0.5,A1*r+10)",
        "=A1*INDIRECT(\"C1\")+10",
        "=HALF(A1)+10",
        "=LAMBDA(x,x*0.5+10)(A1)",
    ] {
        let mut m = new_empty_model();
        m._set("C1", "0.5");
        m._set("C2", "0.2");
        m.new_defined_name("Rate", None, "Sheet1!$C$1").unwrap();
        m.new_defined_name("HALF", None, "LAMBDA(x,x/2)").unwrap();
        m._set("A1", formula);
        m.set_iterative_calculation(true, 100, 1e-9).unwrap();
        assert!(
            m.evaluation_report().converged,
            "{formula}: {:?}",
            m.evaluation_report()
        );
        assert!((number(&m, "A1") - 20.0).abs() < 1e-8);
    }
}

#[test]
fn scalar_dynamic_formulas_really_use_incremental() {
    for formula in [
        "=Rate*2",
        "=INDEX(A1:A2,1)*2",
        "=LET(x,A1,x*2)",
        "=INDIRECT(\"A1\")*2",
        "=DOUBLE(A1)",
        "=LAMBDA(x,x*2)(A1)",
    ] {
        let mut m = new_empty_model();
        m._set("A1", "3");
        m._set("A2", "5");
        m.new_defined_name("Rate", None, "Sheet1!$A$1").unwrap();
        m.new_defined_name("DOUBLE", None, "LAMBDA(x,x*2)").unwrap();
        m._set("B1", formula);
        m._set("C1", "=B1+1");
        m.evaluate_headless();
        for n in [4, 7] {
            m._set("A1", &n.to_string());
            let dirty = m.evaluate_incremental_headless(&[(0, 1, 1)]).unwrap();
            assert!(dirty.contains(&(0, 1, 2)), "{formula}");
            assert_eq!(number(&m, "C1"), (n * 2 + 1) as f64);
        }
        assert_eq!(m.evaluation.dependency_index_builds, 1, "{formula}");
    }
}

#[test]
fn unwind_does_not_poison_lambda_report() {
    for depth in [1, 7, 64] {
        let mut m = new_empty_model();
        m.set_max_evaluation_depth(depth);
        m.new_defined_name("F", None, "LAMBDA(n,IF(n=1,1,n*F(n-1)))")
            .unwrap();
        m._set("A1", "=F(B1)");
        for row in 1..200 {
            m._set(&format!("B{row}"), &format!("=B{}", row + 1));
        }
        m._set("B200", "5");
        m.evaluate_headless();
        assert_eq!(number(&m, "A1"), 120.0);
        assert!(
            m.evaluation_report().converged,
            "{:?}",
            m.evaluation_report()
        );
    }
}

#[test]
fn incremental_preserves_failure_after_unrelated_edit() {
    let mut m = new_empty_model();
    m.set_max_lambda_depth(8);
    let mut body = "1".to_string();
    for _ in 0..9 {
        body = format!("LAMBDA(x,{body})(1)");
    }
    m._set("A1", &format!("=SUM({body})"));
    m.evaluate_headless();
    assert_eq!(
        m.evaluation_report().failure,
        Some(CalculationFailure::RecursionLimit)
    );
    m._set("Z1", "7");
    assert!(m.evaluate_incremental_headless(&[(0, 1, 26)]).is_none());
    assert_eq!(m._get_text("A1"), "#NUM!");
    assert_eq!(
        m.evaluation_report().failure,
        Some(CalculationFailure::RecursionLimit)
    );
    m._set("A1", "=42");
    assert!(m.evaluate_incremental_headless(&[(0, 1, 1)]).is_none());
    assert!(m.evaluation_report().converged);
}

#[test]
#[cfg(not(target_family = "wasm"))]
fn configured_lambda_depth_allows_more_than_64_calls() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let mut m = new_empty_model();
            sumto(&mut m);
            assert_eq!(m.get_max_lambda_depth(), 256);
            m.set_max_evaluation_stack(8 * 1024 * 1024);
            m.set_max_lambda_depth(101);
            for (n, ok) in [(63, true), (64, true), (100, true), (101, false)] {
                m._set("A1", &format!("=SUMTO({n})"));
                m.evaluate_headless();
                assert_eq!(m.evaluation_report().converged, ok, "n={n}");
                if ok {
                    assert_eq!(number(&m, "A1"), (n * (n + 1) / 2) as f64);
                } else {
                    assert_eq!(
                        m.evaluation_report().failure,
                        Some(CalculationFailure::RecursionLimit)
                    );
                }
            }
            m.set_max_lambda_depth(0);
            assert_eq!(m.get_max_lambda_depth(), 1);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
#[cfg(not(target_family = "wasm"))]
fn infinite_lambda_is_bounded_on_a_small_worker_stack() {
    std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(|| {
            let mut m = new_empty_model();
            m.set_max_evaluation_stack(32 * 1024);
            m.new_defined_name("FOREVER", None, "LAMBDA(x,FOREVER(x))")
                .unwrap();
            m._set("A1", "=FOREVER(1)");
            m.evaluate_headless();
            assert_eq!(m._get_text("A1"), "#NUM!");
            assert_eq!(
                m.evaluation_report().failure,
                Some(CalculationFailure::RecursionLimit)
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn boolean_cycles_use_exact_typed_equality() {
    for formula in ["=OR(A1,TRUE)", "=LET(x,A1,OR(x,TRUE))"] {
        let mut m = new_empty_model();
        m._set("A1", formula);
        m.set_iterative_calculation(true, 10, 100.0).unwrap();
        assert!(m.evaluation_report().converged);
        assert_eq!(m.evaluation_report().iterations, 2);
        assert_eq!(
            m.get_cell_value_by_ref("Sheet1!A1").unwrap(),
            CellValue::Boolean(true)
        );
    }
    for formula in ["=NOT(A1)", "=IF(ISLOGICAL(A1),1,TRUE)"] {
        let mut m = new_empty_model();
        m._set("A1", formula);
        m.set_iterative_calculation(true, 10, 100.0).unwrap();
        assert_eq!(
            m.evaluation_report().failure,
            Some(CalculationFailure::IterationLimit)
        );
        assert_eq!(m.evaluation_report().residual, 1.0);
    }
}

#[test]
fn zero_tolerance_and_excel_iteration_limits() {
    let mut m = new_empty_model();
    m._set("A1", "=A1/2+10");
    m.set_iterative_calculation(true, 32767, 0.0).unwrap();
    assert!(m.evaluation_report().converged);
    assert_eq!(number(&m, "A1"), 20.0);
    assert_eq!(m.evaluation_report().residual, 0.0);
    assert!(m.set_iterative_calculation(true, 32768, 0.0).is_err());
    m.workbook.settings.iterative_calculation.enabled = false;
    m.workbook.settings.iterative_calculation.maximum_iterations = 0;
    m._set("A1", "=42");
    m.evaluate_headless();
    assert!(m.evaluation_report().converged);
}

#[test]
fn snapshots_only_visit_cycle_participants() {
    let mut m = new_empty_model();
    m._set("A1", "=A1/2+10");
    for row in 1..=1000 {
        m._set(&format!("B{row}"), "=42");
    }
    m.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert!(m.evaluation_report().converged);
    assert_eq!(
        m.evaluation.cycle_snapshot_cells,
        m.evaluation_report().iterations as usize
    );
}

#[test]
fn unrelated_and_inactive_volatility_does_not_block_cycles() {
    for formula in [
        "=RAND()",
        "=TODAY()",
        "=NOW()",
        "=A1+RAND()",
        "=RANDBETWEEN(1,9)",
        "=RANDARRAY(2)",
    ] {
        let mut m = new_empty_model();
        m._set("A1", "=A1/2+10");
        m._set("E5", formula);
        m.set_iterative_calculation(true, 100, 1e-9).unwrap();
        assert!(
            m.evaluation_report().converged,
            "{formula}: {:?}",
            m.evaluation_report()
        );
    }
    let mut m = new_empty_model();
    m._set("E5", "=RAND()");
    m._set("E5", "7");
    m.new_defined_name("UNUSED", None, "LAMBDA(x,RAND())")
        .unwrap();
    m._set("A1", "=A1/2+IF(FALSE,RAND(),10)");
    m.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert!(m.evaluation_report().converged);
}

#[test]
fn volatility_reaches_cycles_through_actual_dependencies() {
    for source in [
        "=RAND()",
        "=RANDBETWEEN(1,9)",
        "=RANDARRAY(1)",
        "=SAMPLE(1)",
    ] {
        for cycle in [
            "=D1/2+A1",
            "=D1/2+SUM(A1:A2)",
            "=D1/2+INDIRECT(\"A1\")",
            "=D1/2+Input",
        ] {
            let mut m = new_empty_model();
            m.new_defined_name("SAMPLE", None, "LAMBDA(x,RAND())")
                .unwrap();
            m.new_defined_name("Input", None, "Sheet1!$A$1").unwrap();
            m._set("A1", source);
            m._set("A2", "1");
            m._set("D1", cycle);
            m.set_iterative_calculation(true, 100, 1e-9).unwrap();
            assert_eq!(
                m.evaluation_report().failure,
                Some(CalculationFailure::VolatileCycle),
                "{source}, {cycle}"
            );
        }
    }
    let mut m = new_empty_model();
    m._set("A1", "=RANDARRAY(2)");
    m._set("D1", "=D1/2+A2");
    m.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert_eq!(
        m.evaluation_report().failure,
        Some(CalculationFailure::VolatileCycle)
    );
}

#[test]
fn spill_growth_shrink_and_blocking_fall_back_safely() {
    let mut m = new_empty_model();
    m._set("A1", "1");
    m._set("B1", "=SEQUENCE(A1)");
    m._set("D1", "=SUM(B1:B5)");
    m.evaluate_headless();
    assert!(m.evaluate_incremental_headless(&[]).is_some());
    for n in [3, 2, 1] {
        m._set("A1", &n.to_string());
        assert!(m.evaluate_incremental_headless(&[(0, 1, 1)]).is_none());
        assert_eq!(number(&m, "D1"), (n * (n + 1) / 2) as f64);
    }
    assert!(m.evaluate_incremental_headless(&[]).is_some());
    m._set("B2", "99");
    m._set("A1", "2");
    assert!(m
        .evaluate_incremental_headless(&[(0, 2, 2), (0, 1, 1)])
        .is_none());
    assert_eq!(m._get_text("B1"), "#SPILL!");
    let builds = m.evaluation.dependency_index_builds;
    for _ in 0..3 {
        assert!(m.evaluate_incremental_headless(&[]).is_none());
    }
    assert_eq!(m.evaluation.dependency_index_builds, builds);
    m._set("B2", "");
    assert!(m.evaluate_incremental_headless(&[(0, 2, 2)]).is_none());
    assert_eq!(number(&m, "D1"), 3.0);
    m._set("B1", "=42");
    assert!(m.evaluate_incremental_headless(&[(0, 1, 2)]).is_none());
    assert!(m.evaluate_incremental_headless(&[]).is_some());
}

#[test]
fn structural_operations_invalidate_incremental_baseline() {
    let mut m = new_empty_model();
    m._set("A1", "1");
    m._set("B1", "=A1*10");
    m.evaluate_headless();
    assert!(m.evaluate_incremental_headless(&[]).is_some());
    m.insert_rows(0, 1, 1).unwrap();
    m._set("A2", "7");
    assert!(m.evaluate_incremental_headless(&[(0, 2, 1)]).is_none());
    assert_eq!(number(&m, "B2"), 70.0);
    m.insert_columns(0, 1, 1).unwrap();
    m._set("B2", "8");
    assert!(m.evaluate_incremental_headless(&[(0, 2, 2)]).is_none());
    assert_eq!(number(&m, "C2"), 80.0);
    m.delete_rows(0, 1, 1).unwrap();
    m.delete_columns(0, 1, 1).unwrap();
    m._set("A1", "9");
    assert!(m.evaluate_incremental_headless(&[(0, 1, 1)]).is_none());
    assert_eq!(number(&m, "B1"), 90.0);
    m.move_rows_action(0, 1, 1, 1).unwrap();
    m._set("A2", "11");
    assert!(m.evaluate_incremental_headless(&[(0, 2, 1)]).is_none());
    assert_eq!(number(&m, "B2"), 110.0);
    m.move_columns_action(0, 1, 1, 2).unwrap();
    m._set("C2", "12");
    assert!(m.evaluate_incremental_headless(&[(0, 2, 3)]).is_none());
    assert_eq!(number(&m, "A2"), 120.0);
}

#[test]
fn fork_preserves_limits_and_dynamic_links() {
    let mut m = new_empty_model();
    m._set("A1", "=HYPERLINK(\"https://example.com\",\"Example\")");
    m.set_max_evaluation_depth(7);
    m.set_max_evaluation_stack(12345);
    m.set_max_lambda_depth(19);
    m.evaluate_headless();
    let mut fork = m.fork_evaluation_session().unwrap();
    assert_eq!(fork.get_max_evaluation_depth(), 7);
    assert_eq!(fork.get_max_evaluation_stack(), 12345);
    assert_eq!(fork.get_max_lambda_depth(), 19);
    assert_eq!(
        fork.get_links_list(0).unwrap(),
        m.get_links_list(0).unwrap()
    );
    fork._set("Z1", "7");
    assert!(fork.evaluate_incremental_headless(&[(0, 1, 26)]).is_some());
    assert_eq!(fork.get_links_list(0).unwrap().len(), 1);
    fn assert_send<T: Send>() {}
    assert_send::<Model>();
}

#[test]
fn newly_discovered_cycle_members_need_a_comparison() {
    let mut m = new_empty_model();
    m._set("A1", "=IF(A1<1,1,B1*0+1)");
    m._set("B1", "=IF(A1<1,7,0)");
    m.set_iterative_calculation(true, 2, 0.0).unwrap();
    assert_eq!(
        m.evaluation_report().failure,
        Some(CalculationFailure::IterationLimit)
    );
    m.set_iterative_calculation(true, 3, 0.0).unwrap();
    assert!(m.evaluation_report().converged);
}

#[test]
fn abandoned_pass_discards_volatility_and_recursion_diagnostics() {
    for abandoned in ["RAND()", "FOREVER(1)"] {
        let mut m = new_empty_model();
        m.set_max_lambda_depth(8);
        m.new_defined_name("FOREVER", None, "LAMBDA(x,FOREVER(x))")
            .unwrap();
        m._set("A1", "=A1/2+B1");
        m._set("B1", &format!("=LET(x,C2,IF(x=0,{abandoned},10))"));
        m._set("C1", "=SEQUENCE(2)");
        m.set_iterative_calculation(true, 100, 1e-9).unwrap();
        assert!(m.evaluation.restarts_in_last_evaluation > 0);
        assert!(
            m.evaluation_report().converged,
            "{abandoned}: {:?}",
            m.evaluation_report()
        );
        assert!((number(&m, "A1") - 20.0).abs() < 1e-8);
    }
}

#[test]
fn cse_anchors_use_full_evaluation_without_building_an_index() {
    let mut m = new_empty_model();
    m._set("A1", "3");
    m.set_user_array_formula(0, 1, 2, 1, 1, "=A1*2").unwrap();
    m._set("C1", "=B1+1");
    m.evaluate_headless();
    for n in [4, 7] {
        m._set("A1", &n.to_string());
        assert!(m.evaluate_incremental_headless(&[(0, 1, 1)]).is_none());
        assert_eq!(number(&m, "C1"), (n * 2 + 1) as f64);
    }
    assert_eq!(m.evaluation.dependency_index_builds, 0);
}

#[test]
fn genuine_array_and_cse_cycles_still_fail() {
    let mut m = new_empty_model();
    m._set("A1", "=SEQUENCE(2,1,A1/2+10)");
    m.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert_eq!(
        m.evaluation_report().failure,
        Some(CalculationFailure::CyclicArray)
    );
    let mut m = new_empty_model();
    m.set_user_array_formula(0, 1, 1, 1, 1, "=A1/2+10").unwrap();
    m.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert_eq!(
        m.evaluation_report().failure,
        Some(CalculationFailure::CyclicArray)
    );
}

#[test]
fn name_and_sheet_changes_cannot_reuse_stale_dependencies() {
    let mut m = new_empty_model();
    m._set("A1", "2");
    m._set("A2", "3");
    m.new_defined_name("Input", None, "Sheet1!$A$1").unwrap();
    m._set("B1", "=Input*10");
    m.evaluate_headless();
    assert!(m.evaluate_incremental_headless(&[]).is_some());
    m.update_defined_name("Input", None, "Input", None, "Sheet1!$A$2")
        .unwrap();
    m._set("A2", "4");
    m.evaluate_incremental_headless(&[(0, 2, 1)]);
    assert_eq!(number(&m, "B1"), 40.0);
    m.add_sheet("Other").unwrap();
    m.rename_sheet("Sheet1", "Renamed").unwrap();
    m.move_sheet(0, 1).unwrap();
    m.set_user_input(1, 2, 1, "5".into()).unwrap();
    m.evaluate_incremental_headless(&[(1, 2, 1)]);
    assert_eq!(
        m.get_cell_value_by_ref("Renamed!B1").unwrap(),
        CellValue::Number(50.0)
    );
    m.delete_sheet(0).unwrap();
    m.set_user_input(0, 2, 1, "6".into()).unwrap();
    m.evaluate_incremental_headless(&[(0, 2, 1)]);
    assert_eq!(
        m.get_cell_value_by_ref("Renamed!B1").unwrap(),
        CellValue::Number(60.0)
    );
    m.delete_defined_name("Input", None).unwrap();
    m.evaluate_incremental_headless(&[]);
    assert_eq!(m.get_formatted_cell_value(0, 1, 2).unwrap(), "#NAME?");
}

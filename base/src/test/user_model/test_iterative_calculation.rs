#![allow(clippy::unwrap_used)]

use crate::test::user_model::util::new_empty_user_model;
use crate::types::IterativeCalculation;
use crate::UserModel;

fn value(model: &UserModel, row: i32, column: i32) -> f64 {
    model
        .get_formatted_cell_value(0, row, column)
        .unwrap()
        .parse()
        .unwrap()
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-6,
        "expected {expected}, got {actual}"
    );
}

#[test]
fn set_and_get() {
    let mut model = new_empty_user_model();
    assert_eq!(
        model.get_iterative_calculation(),
        IterativeCalculation::default()
    );
    model.set_iterative_calculation(true, 30, 0.01).unwrap();
    assert_eq!(
        model.get_iterative_calculation(),
        IterativeCalculation {
            enabled: true,
            maximum_iterations: 30,
            maximum_change: 0.01,
        }
    );
}

#[test]
fn undo_and_redo() {
    let mut model = new_empty_user_model();
    model.set_user_input(0, 1, 1, "=B1/2+10").unwrap();
    model.set_user_input(0, 1, 2, "=A1+5").unwrap();
    assert_eq!(model.get_formatted_cell_value(0, 1, 1).unwrap(), "#CIRC!");

    model.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert_close(value(&model, 1, 1), 25.0);
    assert_close(value(&model, 1, 2), 30.0);

    model.undo().unwrap();
    assert_eq!(
        model.get_iterative_calculation(),
        IterativeCalculation::default()
    );
    assert_eq!(model.get_formatted_cell_value(0, 1, 1).unwrap(), "#CIRC!");

    model.redo().unwrap();
    assert_eq!(
        model.get_iterative_calculation(),
        IterativeCalculation {
            enabled: true,
            maximum_iterations: 100,
            maximum_change: 1e-9,
        }
    );
    assert_close(value(&model, 1, 1), 25.0);
}

#[test]
fn survives_a_round_trip_through_bytes() {
    let mut model = new_empty_user_model();
    model.set_iterative_calculation(true, 12, 0.25).unwrap();
    let bytes = model.to_bytes();
    let restored = UserModel::from_bytes(&bytes, "en").unwrap();
    assert_eq!(
        restored.get_iterative_calculation(),
        IterativeCalculation {
            enabled: true,
            maximum_iterations: 12,
            maximum_change: 0.25,
        }
    );
}

#[test]
fn settings_and_history_respect_paused_evaluation() {
    let mut m = new_empty_user_model();
    m.set_user_input(0, 1, 1, "=A1/2+10").unwrap();
    m.pause_evaluation();
    m.set_iterative_calculation(true, 100, 1e-9).unwrap();
    assert_eq!(m.model.evaluation_report().iterations, 0);
    assert_eq!(m.get_formatted_cell_value(0, 1, 1).unwrap(), "#CIRC!");
    m.evaluate();
    assert_close(value(&m, 1, 1), 20.0);
    m.undo().unwrap();
    assert_eq!(m.model.evaluation_report().iterations, 0);
    assert_close(value(&m, 1, 1), 20.0);
    m.redo().unwrap();
    assert_eq!(m.model.evaluation_report().iterations, 0);
    m.resume_evaluation();
    m.undo().unwrap();
    assert_eq!(m.get_formatted_cell_value(0, 1, 1).unwrap(), "#CIRC!");
    m.redo().unwrap();
    assert_close(value(&m, 1, 1), 20.0);
}

#[test]
fn mixed_setting_and_cell_batch_evaluates_only_once() {
    use crate::user_model::history::{Diff, DiffType, QueueDiffs};
    let mut m = new_empty_user_model();
    let list = vec![
        Diff::SetIterativeCalculation {
            old_value: IterativeCalculation::default(),
            new_value: IterativeCalculation {
                enabled: true,
                ..IterativeCalculation::default()
            },
        },
        Diff::SetCellValue {
            sheet: 0,
            row: 1,
            column: 1,
            new_value: "=A1/2+10".into(),
            old_value: Box::new(None),
        },
    ];
    let before = m.model.evaluation.full_evaluations;
    m.apply_external_diffs(&bitcode::encode(&vec![QueueDiffs {
        r#type: DiffType::Redo,
        list: list.clone(),
    }]))
    .unwrap();
    assert_eq!(m.model.evaluation.full_evaluations, before + 1);
    assert!(m.model.evaluation_report().converged);
    m.apply_external_diffs(&bitcode::encode(&vec![QueueDiffs {
        r#type: DiffType::Undo,
        list: list.clone(),
    }]))
    .unwrap();
    assert_eq!(m.model.evaluation.full_evaluations, before + 2);
    m.pause_evaluation();
    m.apply_external_diffs(&bitcode::encode(&vec![QueueDiffs {
        r#type: DiffType::Redo,
        list,
    }]))
    .unwrap();
    assert_eq!(m.model.evaluation.full_evaluations, before + 2);
}

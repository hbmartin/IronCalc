#![allow(clippy::unwrap_used, clippy::panic)]
use super::history::{Diff, QueueDiffs};
use crate::{
    expressions::types::Area,
    mock_time::{clock_reads, with_advancing_time},
    test::util::new_empty_model,
    types::{ArrayKind, CalculationFailure, Cell, Workbook},
    Model, UserModel,
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
fn peer(m: &UserModel) -> UserModel<'static> {
    UserModel::from_model(Model::from_bytes(&m.to_bytes(), "en").unwrap())
}
fn select(m: &mut UserModel, r1: i32, c1: i32, r2: i32, c2: i32) {
    m.set_selected_cell(r1, c1).unwrap();
    m.set_selected_range(r1, c1, r2, c2).unwrap();
}
fn val(m: &UserModel, r: i32, c: i32) -> String {
    m.get_formatted_cell_value(0, r, c).unwrap()
}
fn cse(m: &UserModel, r: i32, c: i32, w: i32, h: i32) {
    assert!(
        matches!(m.model.workbook.worksheets[0].cell(r,c).as_deref(),Some(Cell::ArrayFormula {kind:ArrayKind::Cse,r:extent,..}) if *extent==(w,h))
    );
}

#[test]
fn cse_cuts_and_rewrites_preserve_groups_in_history_and_collaboration() {
    for paused in [false, true] {
        for height in [1, 2] {
            for target in [2, 4] {
                let mut m = UserModel::from_model(new_empty_model());
                m.set_user_array_formula(0, 1, 1, 1, height, "=123")
                    .unwrap();
                let mut p = peer(&m);
                m.flush_send_queue();
                select(&mut m, 1, 1, height, 1);
                let cp = m.copy_to_clipboard().unwrap();
                m.set_selected_cell(target, 1).unwrap();
                if paused {
                    m.pause_evaluation();
                    p.pause_evaluation();
                }
                m.paste_from_clipboard(0, cp.range, &cp.data, true).unwrap();
                p.apply_external_diffs(&m.flush_send_queue()).unwrap();
                cse(&m, target, 1, 1, height);
                cse(&p, target, 1, 1, height);
                assert_eq!(val(&m, target, 1), "123");
                m.pause_evaluation();
                m.undo().unwrap();
                cse(&m, 1, 1, 1, height);
                assert_eq!(val(&m, 1, 1), "123");
                m.redo().unwrap();
                cse(&m, target, 1, 1, height);
                assert_eq!(val(&m, target, 1), "123");
                m.evaluate();
                p.evaluate();
                assert_eq!(val(&p, target, 1), "123");
                let saved: Workbook = bitcode::decode(&m.to_bytes()).unwrap();
                assert!(matches!(
                    saved.worksheets[0].cell(target, 1).as_deref(),
                    Some(Cell::ArrayFormula {
                        kind: ArrayKind::Cse,
                        ..
                    })
                ));
            }
        }
    }
    for paused in [false, true] {
        let mut m = UserModel::from_model(new_empty_model());
        m.set_user_input(0, 1, 1, "2").unwrap();
        m.set_user_input(0, 2, 1, "3").unwrap();
        m.set_user_array_formula(0, 1, 4, 1, 2, "=A1:A2*2").unwrap();
        let mut p = peer(&m);
        m.flush_send_queue();
        select(&mut m, 1, 1, 2, 1);
        let cp = m.copy_to_clipboard().unwrap();
        m.set_selected_cell(3, 3).unwrap();
        if paused {
            m.pause_evaluation();
        }
        m.paste_from_clipboard(0, cp.range, &cp.data, true).unwrap();
        p.apply_external_diffs(&m.flush_send_queue()).unwrap();
        cse(&m, 1, 4, 1, 2);
        assert_eq!(val(&m, 2, 4), "6");
        assert_eq!(m.get_cell_content(0, 1, 4).unwrap(), "=C3:C4*2");
        cse(&p, 1, 4, 1, 2);
        m.pause_evaluation();
        m.undo().unwrap();
        assert_eq!(m.get_cell_content(0, 1, 4).unwrap(), "=A1:A2*2");
        m.redo().unwrap();
        m.evaluate();
        assert_eq!(val(&m, 2, 4), val(&p, 2, 4));
    }
}

#[test]
fn translated_merge_discards_covered_rewrites_and_cse_merges_replay_as_arrays() {
    for paused in [false, true] {
        let mut m = UserModel::from_model(new_empty_model());
        m.set_user_input(0, 1, 1, "2").unwrap();
        m.merge_cells(&area(1, 1, 2, 1)).unwrap();
        m.set_selected_cell(1, 1).unwrap();
        let cp = m.copy_to_clipboard().unwrap();
        assert_eq!(cp.range, (1, 1, 1, 1));
        m.set_user_input(0, 5, 5, "=A1*2").unwrap();
        let mut p = peer(&m);
        m.flush_send_queue();
        m.set_selected_cell(5, 4).unwrap();
        if paused {
            m.pause_evaluation();
        }
        m.paste_from_clipboard(0, cp.range, &cp.data, true).unwrap();
        p.apply_external_diffs(&m.flush_send_queue()).unwrap();
        assert_eq!(val(&m, 5, 4), "2");
        assert_eq!(m.get_cell_content(0, 5, 5).unwrap(), "");
        m.pause_evaluation();
        m.undo().unwrap();
        assert_eq!(val(&m, 5, 5), "4");
        m.redo().unwrap();
        assert_eq!(val(&m, 5, 5), "");
        let mut m = UserModel::from_model(new_empty_model());
        m.set_user_array_formula(0, 1, 2, 1, 1, "=123").unwrap();
        let mut p = peer(&m);
        m.flush_send_queue();
        if paused {
            m.pause_evaluation();
        }
        m.merge_cells(&area(1, 1, 2, 1)).unwrap();
        p.apply_external_diffs(&m.flush_send_queue()).unwrap();
        cse(&m, 1, 1, 1, 1);
        cse(&p, 1, 1, 1, 1);
        m.pause_evaluation();
        m.undo().unwrap();
        cse(&m, 1, 2, 1, 1);
        m.redo().unwrap();
        cse(&m, 1, 1, 1, 1);
    }
}

#[test]
fn failed_preparation_is_atomic_and_successful_preparation_reads_no_clock() {
    for paused in [false, true] {
        with_advancing_time(1_700_000_000_000, 1000, || {
            let mut m = UserModel::from_model(new_empty_model());
            m.set_user_input(0, 1, 1, "1").unwrap();
            m.set_user_input(0, 2, 1, "2").unwrap();
            m.set_user_array_formula(0, 3, 3, 1, 3, "=123").unwrap();
            select(&mut m, 1, 1, 2, 1);
            let cp = m.copy_to_clipboard().unwrap();
            m.set_selected_cell(4, 3).unwrap();
            if paused {
                m.pause_evaluation();
            }
            m.flush_send_queue();
            let bytes = m.to_bytes();
            let report = m.model.evaluation_report().clone();
            let view = m.get_selected_view();
            let reads = clock_reads();
            let history = m.history.undo_stack.len();
            let index = m.model.dependency_index.clone();
            assert!(m.paste_from_clipboard(0, cp.range, &cp.data, true).is_err());
            assert_eq!(m.to_bytes(), bytes);
            assert_eq!(*m.model.evaluation_report(), report);
            assert_eq!(m.get_selected_view(), view);
            assert_eq!(clock_reads(), reads);
            assert_eq!(m.history.undo_stack.len(), history);
            assert!(bitcode::decode::<Vec<QueueDiffs>>(&m.flush_send_queue())
                .unwrap()
                .is_empty());
            assert_eq!(index.is_some(), m.model.dependency_index.is_some());
            let mut bad = serde_json::to_value(&cp.data).unwrap();
            bad["3"] = bad["1"].clone();
            let bad = serde_json::from_value(bad).unwrap();
            m.set_selected_cell(10, 1).unwrap();
            let before = m.to_bytes();
            assert!(m.paste_from_clipboard(0, cp.range, &bad, false).is_err());
            assert_eq!(m.to_bytes(), before);
            assert_eq!(clock_reads(), reads);
            assert!(bitcode::decode::<Vec<QueueDiffs>>(&m.flush_send_queue())
                .unwrap()
                .is_empty());
            m.paste_from_clipboard(0, cp.range, &cp.data, true).unwrap();
            assert_eq!(clock_reads(), reads + usize::from(!paused));
        });
    }
}

#[test]
fn remote_sheet_id_reuse_and_merges_retire_both_history_owners() {
    let mut m = UserModel::from_model(new_empty_model());
    let mut p = peer(&m);
    m.set_language("fr").unwrap();
    m.new_sheet().unwrap();
    m.set_language("en").unwrap();
    m.set_user_input(1, 1, 1, "=SEQUENCE(3)").unwrap();
    m.undo().unwrap();
    m.undo().unwrap();
    m.flush_send_queue();
    p.new_sheet().unwrap();
    m.apply_external_diffs(&p.flush_send_queue()).unwrap();
    m.pause_evaluation();
    m.redo().unwrap();
    m.redo().unwrap();
    assert!(!matches!(
        m.model.workbook.worksheets[2].cell(2, 1).as_deref(),
        Some(Cell::SpillCell { .. })
    ));
    m.set_user_input(2, 2, 1, "99").unwrap();
    for redo in [false, true] {
        let mut m = UserModel::from_model(new_empty_model());
        let mut p = peer(&m);
        m.set_user_input(0, 1, 1, "=SEQUENCE(3)").unwrap();
        if redo {
            m.undo().unwrap();
        }
        p.merge_cells(&area(2, 1, 2, 1)).unwrap();
        m.apply_external_diffs(&p.flush_send_queue()).unwrap();
        m.pause_evaluation();
        if redo {
            m.redo().unwrap();
        } else {
            m.undo().unwrap();
        }
        assert!(!matches!(
            m.model.workbook.worksheets[0].cell(2, 1).as_deref(),
            Some(Cell::SpillCell { .. })
        ));
    }
}

#[test]
fn sparse_structural_history_preserves_unmoved_remote_dependents() {
    let mut raw = new_empty_model();
    for col in 1..=1000 {
        raw.set_user_input(0, 1, col, "=1".into()).unwrap();
    }
    raw._set("A2", "1");
    raw._set("B2", "=A2*2");
    raw._set("A20", "=ROW()");
    raw.evaluate();
    let mut m = UserModel::from_model(raw);
    m.insert_rows(0, 10, 1).unwrap();
    assert!(m.history.undo_stack.last().unwrap().display.cell_count() <= 2);
    let mut p = peer(&m);
    p.set_user_input(0, 2, 1, "10").unwrap();
    m.apply_external_diffs(&p.flush_send_queue()).unwrap();
    assert_eq!(val(&m, 2, 2), "20");
    m.pause_evaluation();
    m.undo().unwrap();
    assert_eq!(val(&m, 2, 2), "20");
    assert_eq!(val(&m, 20, 1), "20");
}

#[test]
fn remote_batches_prune_once_and_deduplicate_spill_rectangles() {
    let mut m = UserModel::from_model(new_empty_model());
    let mut p = peer(&m);
    for row in 1..=100 {
        m.set_user_input(0, row, 1, "=1").unwrap();
    }
    let count = m.history.undo_stack.len();
    for row in 1..=20 {
        p.set_rows_height(0, row, row, 25.0).unwrap();
    }
    m.apply_external_diffs(&p.flush_send_queue()).unwrap();
    assert_eq!(m.model.evaluation.display.history_scans, 0);
    for row in 1..=20 {
        p.set_user_input(0, row, 1, "=2").unwrap();
    }
    m.apply_external_diffs(&p.flush_send_queue()).unwrap();
    assert_eq!(m.model.evaluation.display.conflict_batches, 1);
    assert_eq!(m.model.evaluation.display.history_scans, count);
    m.set_user_input(0, 1, 3, "=SEQUENCE(1000)").unwrap();
    let mut conflicts = crate::display::DisplayConflicts::default();
    conflicts.area(&m.model, &area(1, 3, 1, 1000));
    assert_eq!(conflicts.areas.len(), 1);
}

#[test]
fn scalar_clears_keep_only_the_incremental_baseline_and_noop_batches_skip_rules() {
    for all in [false, true] {
        let mut m = new_empty_model();
        m._set("A1", "1");
        m._set("B1", "=A1*2");
        m.evaluate();
        m.evaluate_incremental(&[]);
        if all {
            m.range_clear_all(&area(1, 1, 1, 1)).unwrap();
        } else {
            m.range_clear_contents(&area(1, 1, 1, 1)).unwrap();
        }
        assert_eq!(
            m.evaluation_report().failure,
            Some(CalculationFailure::NotEvaluated)
        );
        assert!(m.fork_evaluation_session().is_err());
        assert!(m.evaluate_incremental(&[(0, 1, 1)]).is_some());
        assert_eq!(m._get_text("B1"), "0");
        let evals = m.evaluation.cf_evaluations;
        m.unmerge_cells(&area(10, 10, 2, 1)).unwrap();
        assert_eq!(m.evaluation.cf_evaluations, evals);
        m._set("C1", "=SEQUENCE(3)");
        m.evaluate();
        m.range_clear_contents(&area(2, 3, 1, 1)).unwrap();
        assert!(m.evaluate_incremental(&[(0, 2, 3)]).is_none());
    }
    let mut raw = new_empty_model();
    raw.evaluate();
    let mut m = UserModel::from_model(raw);
    m.pause_evaluation();
    let cf = m.model.evaluation.cf_evaluations;
    m.set_require_two_iterative_passes(false);
    m.set_columns_width(0, 1, 1, 80.0).unwrap();
    assert_eq!(m.model.evaluation.cf_evaluations, cf);
}

#[test]
fn dynamic_merge_moves_only_the_anchor_cache_and_cross_sheet_cut_preserves_references() {
    for variant in 0..4 {
        let mut m = new_empty_model();
        m._set("B1", "=SEQUENCE(3)");
        m.evaluate();
        let a = area(1, 1, 2, 1);
        match variant {
            0 => m.merge_cells(&a),
            1 => m.merge_cells_center(&a),
            2 => m.merge_cells_across(&a),
            _ => m.merge_cells_down(&area(1, 2, 1, 2)),
        }
        .unwrap();
        if variant != 3 {
            assert!(matches!(
                m.workbook.worksheets[0].cell(1, 1).as_deref(),
                Some(Cell::ArrayFormula { r: (1, 1), .. })
            ));
            assert_eq!(m._get_text("A1"), "1");
        }
        assert!(
            !matches!(
                m.workbook.worksheets[0].cell(3, 2).as_deref(),
                Some(Cell::SpillCell { .. })
            ) || variant == 3
        );
        m.evaluate();
    }
    let mut m = UserModel::from_model(new_empty_model());
    m.new_sheet().unwrap();
    m.set_selected_sheet(0).unwrap();
    m.set_user_input(0, 5, 1, "=C5").unwrap();
    m.set_selected_cell(5, 1).unwrap();
    let cp = m.copy_to_clipboard().unwrap();
    m.set_selected_sheet(1).unwrap();
    m.set_selected_cell(5, 1).unwrap();
    m.paste_from_clipboard(0, cp.range, &cp.data, true).unwrap();
    assert_eq!(m.get_cell_content(1, 5, 1).unwrap(), "=Sheet1!C5");
}

#[cfg(panic = "unwind")]
#[test]
fn formatting_panic_keeps_the_complete_cache_and_recovers_on_neutral_edit() {
    use crate::{
        cf_types::CfRuleInput,
        types::{Dxf, DxfFont},
    };
    let mut raw = new_empty_model();
    raw.add_conditional_formatting(
        0,
        "B1",
        CfRuleInput::Formula {
            formula: "TRUE".into(),
            format: Dxf {
                font: Some(DxfFont {
                    b: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
            stop_if_true: false,
        },
    )
    .unwrap();
    raw.evaluate();
    let mut m = UserModel::from_model(raw);
    m.model.workbook.worksheets[0].conditional_formatting[0].range = "C1".into();
    m.model.evaluation.display.cf_hook = Some(|| panic!("formatting hook"));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        m.with_user_display_edit(false, |m| {
            m.model.evaluate_conditional_formatting();
            Ok(())
        })
        .unwrap();
    }));
    assert!(result.is_err());
    assert!(
        m.model
            .get_extended_style_for_cell(0, 1, 2)
            .unwrap()
            .style
            .font
            .b
    );
    assert!(
        !m.model
            .get_extended_style_for_cell(0, 1, 3)
            .unwrap()
            .style
            .font
            .b
    );
    assert!(m.model.evaluation.display.formatting_recovery);
    m.set_columns_width(0, 1, 1, 90.0).unwrap();
    assert!(
        !m.model
            .get_extended_style_for_cell(0, 1, 2)
            .unwrap()
            .style
            .font
            .b
    );
    assert!(
        m.model
            .get_extended_style_for_cell(0, 1, 3)
            .unwrap()
            .style
            .font
            .b
    );
    assert!(!m.model.evaluation.display.formatting_recovery);
    m.evaluate();
    assert!(m.model.fork_evaluation_session().is_ok());
}

#[test]
fn paused_cut_preparation_ignores_unrelated_cells_and_keeps_their_links() {
    let mut raw = new_empty_model();
    for r in 100..=1100 {
        raw.set_user_input(0, r, 1, "=1".into()).unwrap();
    }
    raw._set("A1", "=HYPERLINK(\"https://moved.example\",\"moved\")");
    raw._set("D1", "=HYPERLINK(\"https://kept.example\",\"kept\")");
    raw.evaluate();
    let mut m = UserModel::from_model(raw);
    m.pause_evaluation();
    select(&mut m, 1, 1, 1, 1);
    let cp = m.copy_to_clipboard().unwrap();
    m.set_selected_cell(10, 2).unwrap();
    m.paste_from_clipboard(0, cp.range, &cp.data, true).unwrap();
    assert!(m.model.evaluation.display.paste_cells <= 3);
    assert!(m.model.links.contains_key(&(0, 1, 4)));
    assert!(m.model.links.contains_key(&(0, 10, 2)));
    assert!(!m.model.links.contains_key(&(0, 1, 1)));
    m.undo().unwrap();
    assert!(m.model.links.contains_key(&(0, 1, 4)));
    assert!(m.model.links.contains_key(&(0, 1, 1)));
    m.redo().unwrap();
    assert!(m.model.links.contains_key(&(0, 1, 4)));
}

#[test]
fn iteration_setting_batches_alone_are_allowed_while_paused() {
    use super::history::DiffType;
    let mut m = UserModel::from_model(new_empty_model());
    m.pause_evaluation();
    let settings = crate::types::IterativeCalculation {
        enabled: true,
        maximum_iterations: 100,
        maximum_change: 0.001,
    };
    let payload = bitcode::encode(&vec![QueueDiffs {
        r#type: DiffType::Redo,
        list: vec![Diff::SetIterativeCalculation {
            old_value: Default::default(),
            new_value: settings.clone(),
        }],
    }]);
    m.apply_external_diffs(&payload).unwrap();
    assert_eq!(m.get_iterative_calculation(), settings);
    assert_eq!(
        m.model.evaluation_report().failure,
        Some(CalculationFailure::NotEvaluated)
    );
}

#[test]
fn singular_cse_cut_into_existing_merge_keeps_array_semantics_on_peers() {
    for paused in [false, true] {
        let mut m = UserModel::from_model(new_empty_model());
        m.set_user_array_formula(0, 1, 1, 1, 1, "=123").unwrap();
        m.merge_cells(&area(3, 3, 2, 1)).unwrap();
        let mut p = peer(&m);
        m.flush_send_queue();
        select(&mut m, 1, 1, 1, 1);
        let cp = m.copy_to_clipboard().unwrap();
        m.set_selected_cell(3, 3).unwrap();
        if paused {
            m.pause_evaluation();
        }
        m.paste_from_clipboard(0, cp.range, &cp.data, true).unwrap();
        p.apply_external_diffs(&m.flush_send_queue()).unwrap();
        cse(&m, 3, 3, 1, 1);
        cse(&p, 3, 3, 1, 1);
        assert_eq!(val(&m, 3, 3), "123");
        m.pause_evaluation();
        m.undo().unwrap();
        cse(&m, 1, 1, 1, 1);
        m.redo().unwrap();
        cse(&m, 3, 3, 1, 1);
    }
}

#[test]
fn remote_merge_does_not_retire_unchanged_merge_history() {
    with_advancing_time(1_700_000_000_000, 1000, || {
        for redo in [false, true] {
            let mut raw = new_empty_model();
            raw._set("A1", "=NOW()");
            raw.merge_cells(&area(1, 1, 2, 1)).unwrap();
            raw.evaluate();
            let mut m = UserModel::from_model(raw);
            let mut p = peer(&m);
            let before = val(&m, 1, 1);
            m.set_user_input(0, 1, 3, "1").unwrap();
            let after = val(&m, 1, 1);
            if redo {
                m.undo().unwrap();
            }
            let supplement = if redo {
                m.history.redo_stack.last().unwrap().display.clone()
            } else {
                m.history.undo_stack.last().unwrap().display.clone()
            };
            p.merge_cells(&area(3, 3, 2, 1)).unwrap();
            m.apply_external_diffs(&p.flush_send_queue()).unwrap();
            let retained = if redo {
                &m.history.redo_stack.last().unwrap().display
            } else {
                &m.history.undo_stack.last().unwrap().display
            };
            assert!(std::sync::Arc::ptr_eq(&supplement, retained));
            m.pause_evaluation();
            if redo {
                m.redo().unwrap();
                assert_eq!(val(&m, 1, 1), after);
            } else {
                m.undo().unwrap();
                assert_eq!(val(&m, 1, 1), before);
            }
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "release benchmark; no timing threshold"]
#[allow(clippy::print_stdout)]
fn benchmark_paused_population_and_large_cut() {
    let mut m = UserModel::from_model(new_empty_model());
    m.pause_evaluation();
    let start = std::time::Instant::now();
    for row in 1..=5000 {
        m.set_user_input(0, row, 1, "1").unwrap();
    }
    let population = start.elapsed();
    let mut raw = new_empty_model();
    for row in 1..=500 {
        for col in 1..=100 {
            raw.set_user_input(0, row, col, "1".into()).unwrap();
        }
    }
    raw.evaluate();
    let mut m = UserModel::from_model(raw);
    m.pause_evaluation();
    select(&mut m, 1, 1, 100, 100);
    let cp = m.copy_to_clipboard().unwrap();
    m.set_selected_cell(800, 1).unwrap();
    let start = std::time::Instant::now();
    m.paste_from_clipboard(0, cp.range, &cp.data, true).unwrap();
    let cut = start.elapsed();
    assert_eq!(val(&m, 899, 100), "1");
    assert!(m.model.evaluation.display.paste_cells <= 20_000);
    println!("paused population (5000): {population:?}; 100x100 cut in 50000 populated cells: {cut:?}; prepared writes: {}",m.model.evaluation.display.paste_cells);
}

#![allow(clippy::unwrap_used, clippy::panic)]
use crate::{
    mock_time::{clock_reads, with_advancing_time},
    test::util::new_empty_model,
    types::{Cell, Workbook},
    UserModel,
};

fn value(model: &UserModel, row: i32, col: i32) -> String {
    model.get_formatted_cell_value(0, row, col).unwrap()
}

#[test]
fn remote_replacement_retires_conflicting_caches_on_undo_and_redo() {
    for redo in [false, true] {
        let mut raw = new_empty_model();
        raw._set("A1", "1");
        raw._set("B1", "=A1+1");
        raw._set("C1", "=A1+10");
        raw.evaluate();
        let mut remote =
            UserModel::from_model(crate::Model::from_bytes(&raw.to_bytes(), "en").unwrap());
        let mut model = UserModel::from_model(raw);
        model.set_user_input(0, 1, 1, "2").unwrap();
        if redo {
            model.undo().unwrap();
        }
        remote.set_user_input(0, 1, 2, "=100").unwrap();
        model
            .apply_external_diffs(&remote.flush_send_queue())
            .unwrap();
        model.pause_evaluation();
        if redo {
            model.redo().unwrap();
        } else {
            model.undo().unwrap();
        }
        assert_eq!(model.get_cell_content(0, 1, 2).unwrap(), "=100");
        assert_eq!(value(&model, 1, 2), "100");
        assert_eq!(value(&model, 1, 3), if redo { "12" } else { "11" });
        let saved: Workbook = bitcode::decode(&model.to_bytes()).unwrap();
        assert!(matches!(
            saved.worksheets[0].cell(1, 2).as_deref(),
            Some(Cell::CellFormula {
                v: crate::types::FormulaValue::Number(100.0),
                ..
            })
        ));
    }
}

#[test]
fn nonpositive_moves_and_zero_displacements_preserve_all_display_state() {
    with_advancing_time(1_700_000_000_000, 1000, || {
        let mut raw = new_empty_model();
        raw._set("A1", "=10");
        raw._set("A2", "=20");
        raw._set("B1", "=30");
        raw.evaluate();
        let bytes = raw.to_bytes();
        let report = raw.evaluation_report().clone();
        let reads = clock_reads();
        for (count, delta) in [(-1, 2), (0, -2), (1, 0)] {
            raw.move_rows_action(0, 2, count, delta).unwrap();
            raw.move_columns_action(0, 2, count, delta).unwrap();
            assert_eq!(raw.to_bytes(), bytes);
            assert_eq!(*raw.evaluation_report(), report);
            assert_eq!(clock_reads(), reads);
        }
        assert!(raw.move_rows_action(99, 2, -1, 2).is_err());
        let mut model = UserModel::from_model(raw);
        let queue = model.flush_send_queue();
        for (count, delta) in [(-1, 2), (0, -2), (1, 0)] {
            model.move_rows_action(0, 2, count, delta).unwrap();
            model.move_columns_action(0, 2, count, delta).unwrap();
        }
        assert_eq!(model.to_bytes(), bytes);
        assert!(!model.can_undo());
        assert_eq!(model.flush_send_queue(), queue);
        assert_eq!(clock_reads(), reads);
    });
}

#[test]
fn rejected_static_array_cut_is_atomic_even_when_target_is_a_spill_child() {
    for paused in [false, true] {
        with_advancing_time(1_700_000_000_000, 1000, || {
            let mut model = UserModel::from_model(new_empty_model());
            model.set_user_array_formula(0, 1, 1, 1, 2, "=123").unwrap();
            model.set_user_input(0, 1, 3, "=SEQUENCE(5)").unwrap();
            model.set_selected_cell(1, 1).unwrap();
            model.set_selected_range(1, 1, 1, 1).unwrap();
            let cp = model.copy_to_clipboard().unwrap();
            model.set_selected_cell(3, 3).unwrap();
            if paused {
                model.pause_evaluation();
            }
            let bytes = model.to_bytes();
            let report = model.model.evaluation_report().clone();
            let selected = model.get_selected_view();
            let reads = clock_reads();
            model.flush_send_queue();
            assert!(model
                .paste_from_clipboard(0, cp.range, &cp.data, true)
                .is_err());
            assert_eq!(model.to_bytes(), bytes);
            assert_eq!(*model.model.evaluation_report(), report);
            assert_eq!(clock_reads(), reads);
            assert_eq!(model.get_selected_view(), selected);
            assert_eq!(value(&model, 5, 3), "5");
            assert!(
                bitcode::decode::<Vec<crate::user_model::history::QueueDiffs>>(
                    &model.flush_send_queue()
                )
                .unwrap()
                .is_empty()
            );
            model.undo().unwrap(); // The prior successful edit, not a failed paste.
            assert_eq!(model.get_cell_content(0, 1, 3).unwrap(), "");
        });
    }
}

#[test]
fn paused_overlapping_partial_spill_cuts_preserve_anchor_and_history() {
    for (end, target) in [(1, 2), (2, 2), (3, 2), (3, 3)] {
        let mut model = UserModel::from_model(new_empty_model());
        model.set_user_input(0, 1, 1, "=SEQUENCE(5)").unwrap();
        model.pause_evaluation();
        model.set_selected_cell(1, 1).unwrap();
        model.set_selected_range(1, 1, end, 1).unwrap();
        let cp = model.copy_to_clipboard().unwrap();
        model.set_selected_cell(target, 1).unwrap();
        model
            .paste_from_clipboard(0, cp.range, &cp.data, true)
            .unwrap();
        assert_eq!(value(&model, target, 1), "1");
        assert_eq!(value(&model, 1, 1), "");
        assert!(model.model.workbook.worksheets[0]
            .sheet_data
            .cells()
            .all(|(_, _, c)| !matches!(*c, Cell::SpillCell { .. })));
        model.undo().unwrap();
        assert_eq!(value(&model, 5, 1), "5");
        model.redo().unwrap();
        assert_eq!(value(&model, target, 1), "1");
        model.evaluate();
        assert_eq!(value(&model, target + 4, 1), "5");
    }
}

#[test]
fn paused_history_and_spill_owner_work_ignore_unrelated_cells() {
    use crate::expressions::types::Area;
    let mut expected_work = None;
    for size in [100, 10_000] {
        let mut raw = new_empty_model();
        for row in 1..=size {
            raw.update_cell_with_number(0, row, 10, 7.0).unwrap();
        }
        raw._set("A1", "=SEQUENCE(5)");
        raw.evaluate();
        let mut model = UserModel::from_model(raw);
        model.pause_evaluation();
        let before = crate::sheet_data::SheetData::cell_visits();
        for row in 1..=10 {
            model.set_user_input(0, row, 2, "1").unwrap();
            assert_eq!(model.model.evaluation.display.journal_positions, 1);
        }
        let edit_work = crate::sheet_data::SheetData::cell_visits() - before;
        let before = crate::sheet_data::SheetData::cell_visits();
        let visits = model.model.evaluation.display.spill_owner_visits;
        model
            .model
            .range_clear_contents(&Area {
                sheet: 0,
                row: 3,
                column: 1,
                width: 1,
                height: 1,
            })
            .unwrap();
        assert_eq!(
            model.model.evaluation.display.spill_owner_visits - visits,
            1
        );
        let clear_work = crate::sheet_data::SheetData::cell_visits() - before;
        if let Some(expected) = expected_work {
            assert_eq!((edit_work, clear_work), expected);
        }
        expected_work = Some((edit_work, clear_work));
        assert_eq!(value(&model, 1, 1), "1");
        assert_eq!(value(&model, 5, 1), "");
    }
}

#[test]
fn cell_storage_journal_covers_all_writes_and_is_detached_from_clones_and_encoding() {
    use crate::types::FormulaValue;
    fn assert_send<T: Send>() {}
    assert_send::<crate::Model<'static>>();
    let mut raw = new_empty_model();
    raw._set("A1", "=1");
    raw._set("B2", "=2");
    raw.evaluate();
    let bytes = raw.to_bytes();
    let cloned = raw.workbook.clone();
    raw.start_display_journal();
    assert_eq!(raw.to_bytes(), bytes);
    assert_eq!(raw.workbook, cloned);
    let mut copy = raw.workbook.clone();
    copy.worksheets[0].sheet_data.remove_row(1);
    let mut fork = raw.fork_evaluation_session().unwrap();
    fork.update_cell_with_formula(0, 1, 1, "=100".into())
        .unwrap();
    fork.evaluate();
    let mut loaded = crate::Model::from_bytes(&raw.to_bytes(), "en").unwrap();
    loaded.update_cell_with_number(0, 1, 1, 100.0).unwrap();
    assert_eq!(raw.take_display_journal().cell_count(), 0);
    raw.start_display_journal();
    let data = &mut raw.workbook.worksheets[0].sheet_data;
    if let Some(Cell::CellFormula { v, .. }) = data.cell_mut(1, 1) {
        *v = FormulaValue::Number(10.0);
    }
    data.set_cell(
        1,
        1,
        Cell::CellFormula {
            f: 0,
            s: 0,
            v: FormulaValue::Number(20.0),
        },
    );
    data.remove_row(2);
    data.set_row(
        3,
        [(
            1,
            Cell::CellFormula {
                f: 0,
                s: 0,
                v: FormulaValue::Number(30.0),
            },
        )],
    );
    let delta = raw.take_display_journal();
    assert_eq!(raw.evaluation.display.journal_positions, 3);
    raw.apply_display_delta(&delta, true);
    assert_eq!(raw._get_text("A1"), "1");
    // Ordinary history restores formula ownership; a display delta only restores its cache.
    assert_eq!(delta.cell_count(), 3);
}

#[test]
fn remote_writes_retire_spill_groups_and_links_even_for_same_formula_text() {
    for same in [false, true] {
        let mut raw = new_empty_model();
        raw._set("A1", "2");
        raw._set("B1", "=SEQUENCE(A1)");
        raw._set("E1", "=HYPERLINK(\"https://old.example/\"&A1,\"link\")");
        raw.evaluate();
        let mut remote =
            UserModel::from_model(crate::Model::from_bytes(&raw.to_bytes(), "en").unwrap());
        let mut model = UserModel::from_model(raw);
        model.set_user_input(0, 1, 1, "3").unwrap();
        remote
            .set_user_input(
                0,
                1,
                2,
                if same {
                    "=SEQUENCE(A1)"
                } else {
                    "=SEQUENCE(1,1,100)"
                },
            )
            .unwrap();
        remote
            .set_user_input(0, 1, 5, "=HYPERLINK(\"https://remote.example\",\"remote\")")
            .unwrap();
        model
            .apply_external_diffs(&remote.flush_send_queue())
            .unwrap();
        model.pause_evaluation();
        model.undo().unwrap();
        assert_eq!(value(&model, 1, 2), if same { "1" } else { "100" });
        assert_eq!(value(&model, 3, 2), if same { "3" } else { "" });
        assert_eq!(
            model.model.links.get(&(0, 1, 5)),
            Some(&crate::types::Link::External {
                target: "https://remote.example".into(),
                tooltip: None
            })
        );
    }
}

#[test]
fn journal_restores_calculation_spills_links_and_nonconvergent_values() {
    let mut raw = new_empty_model();
    raw._set("A1", "3");
    raw._set("B1", "=SEQUENCE(A1)");
    raw._set("E1", "=HYPERLINK(\"https://example.com/\"&A1,\"link\")");
    raw.evaluate();
    let mut model = UserModel::from_model(raw);
    model.set_user_input(0, 1, 1, "5").unwrap();
    assert_eq!(value(&model, 5, 2), "5");
    model.pause_evaluation();
    model.undo().unwrap();
    assert_eq!(value(&model, 3, 2), "3");
    assert_eq!(value(&model, 5, 2), "");
    assert!(
        matches!(model.model.links.get(&(0,1,5)),Some(crate::types::Link::External{target,..}) if target=="https://example.com/3")
    );
    model.redo().unwrap();
    assert_eq!(value(&model, 5, 2), "5");
    assert!(
        matches!(model.model.links.get(&(0,1,5)),Some(crate::types::Link::External{target,..}) if target=="https://example.com/5")
    );
    let mut raw = new_empty_model();
    raw._set("A1", "10");
    raw._set("C1", "=C1/2+A1");
    raw.set_iterative_calculation(true, 100, 0.000001).unwrap();
    let baseline = raw.get_cell_value_by_index(0, 1, 3).unwrap();
    raw.workbook
        .settings
        .iterative_calculation
        .maximum_iterations = 1;
    let mut model = UserModel::from_model(raw);
    model.set_user_input(0, 1, 1, "20").unwrap();
    assert_eq!(
        model.model.evaluation_report().failure,
        Some(crate::types::CalculationFailure::IterationLimit)
    );
    model.pause_evaluation();
    model.undo().unwrap();
    assert_eq!(
        model.model.get_cell_value_by_index(0, 1, 3).unwrap(),
        baseline
    );
    assert_eq!(
        model.model.evaluation_report().failure,
        Some(crate::types::CalculationFailure::NotEvaluated)
    );
}

#[test]
fn journal_tracks_restart_writes_and_sheet_creation_deletion() {
    let mut raw = new_empty_model();
    raw._set("A1", "=SEQUENCE(1,1,SUM(D:D))");
    raw._set("C1", "=SEQUENCE(3,1,10)");
    raw._set("D1", "=SEQUENCE(5)");
    let mut model = UserModel::from_model(raw);
    model.set_user_input(0, 1, 6, "1").unwrap();
    assert_eq!(value(&model, 1, 1), "15");
    assert!(model.model.evaluation.restarts_in_last_evaluation > 0);
    model.pause_evaluation();
    model.undo().unwrap();
    assert!(matches!(
        model.model._get_cell("A1"),
        Cell::ArrayFormula {
            v: crate::types::FormulaValue::Unevaluated,
            ..
        }
    ));
    model.redo().unwrap();
    assert_eq!(value(&model, 1, 1), "15");
    assert_eq!(value(&model, 5, 4), "5");
    model.resume_evaluation();
    model.new_sheet().unwrap();
    model.set_user_input(1, 1, 1, "=SEQUENCE(3)").unwrap();
    model
        .set_user_input(1, 1, 3, "=HYPERLINK(\"https://new.example\",\"new\")")
        .unwrap();
    model.pause_evaluation();
    model.delete_sheet(1).unwrap();
    model.undo().unwrap();
    assert_eq!(model.get_formatted_cell_value(1, 3, 1).unwrap(), "3");
    assert!(model.model.links.contains_key(&(1, 1, 3)));
    model.redo().unwrap();
    assert_eq!(model.model.workbook.worksheets.len(), 1);
}

#[test]
fn rejected_clipboard_bounds_merges_and_payloads_are_atomic() {
    use crate::expressions::types::Area;
    for paused in [false, true] {
        let mut model = UserModel::from_model(new_empty_model());
        for column in 1..=3 {
            model.set_user_input(0, 1, column, "1").unwrap();
        }
        model
            .merge_cells(&Area {
                sheet: 0,
                row: 10,
                column: 2,
                width: 3,
                height: 1,
            })
            .unwrap();
        model.set_selected_cell(1, 1).unwrap();
        model.set_selected_range(1, 1, 1, 3).unwrap();
        let cp = model.copy_to_clipboard().unwrap();
        if paused {
            model.pause_evaluation();
        }
        for (row, column) in [(10, 1), (1, crate::constants::LAST_COLUMN)] {
            model.set_selected_cell(row, column).unwrap();
            let bytes = model.to_bytes();
            let report = model.model.evaluation_report().clone();
            let selected = model.get_selected_view();
            model.flush_send_queue();
            assert!(model
                .paste_from_clipboard(0, cp.range, &cp.data, true)
                .is_err());
            assert_eq!(model.to_bytes(), bytes);
            assert_eq!(*model.model.evaluation_report(), report);
            assert_eq!(model.get_selected_view(), selected);
            let queue = model.flush_send_queue();
            assert!(
                bitcode::decode::<Vec<crate::user_model::history::QueueDiffs>>(&queue)
                    .unwrap()
                    .is_empty()
            );
        }
        // Reject malformed deserialized coordinates before clearing any destination cells.
        let mut payload = serde_json::to_value(&cp.data).unwrap();
        payload["2"] = payload["1"].clone();
        let data = serde_json::from_value(payload).unwrap();
        model.set_selected_cell(20, 1).unwrap();
        let bytes = model.to_bytes();
        assert!(model
            .paste_from_clipboard(0, cp.range, &data, false)
            .is_err());
        assert_eq!(model.to_bytes(), bytes);
    }
}

#[test]
fn remote_structure_retires_rewritten_sheets_but_preserves_unaffected_history() {
    with_advancing_time(1_700_000_000_000, 1000, || {
        let mut raw = new_empty_model();
        raw.new_sheet();
        raw.new_sheet();
        raw._set("Sheet1!A1", "1");
        raw._set("Sheet2!A1", "=Sheet1!A1+10");
        raw._set("Sheet3!A1", "=NOW()");
        raw.evaluate();
        let initial = raw.get_cell_value_by_index(2, 1, 1).unwrap();
        let mut remote =
            UserModel::from_model(crate::Model::from_bytes(&raw.to_bytes(), "en").unwrap());
        let mut model = UserModel::from_model(raw);
        model.set_user_input(0, 1, 1, "2").unwrap();
        remote.insert_rows(0, 1, 1).unwrap();
        model
            .apply_external_diffs(&remote.flush_send_queue())
            .unwrap();
        let remote_clock = model.model.get_cell_value_by_index(2, 1, 1).unwrap();
        assert_ne!(remote_clock, initial);
        model.pause_evaluation();
        model.undo().unwrap();
        assert_eq!(model.get_formatted_cell_value(1, 1, 1).unwrap(), "12");
        assert_eq!(
            model.model.get_cell_value_by_index(2, 1, 1).unwrap(),
            initial
        );
    });
}

#[test]
fn remote_global_context_retires_all_display_supplements() {
    with_advancing_time(1_700_000_000_000, 1000, || {
        let mut raw = new_empty_model();
        raw._set("A1", "1");
        raw._set("B1", "=NOW()");
        raw.evaluate();
        let mut remote =
            UserModel::from_model(crate::Model::from_bytes(&raw.to_bytes(), "en").unwrap());
        let mut model = UserModel::from_model(raw);
        model.set_user_input(0, 1, 1, "2").unwrap();
        remote.set_timezone("Asia/Tokyo").unwrap();
        model
            .apply_external_diffs(&remote.flush_send_queue())
            .unwrap();
        let current = model.model.get_cell_value_by_index(0, 1, 2).unwrap();
        model.pause_evaluation();
        model.undo().unwrap();
        assert_eq!(
            model.model.get_cell_value_by_index(0, 1, 2).unwrap(),
            current
        );
    });
}

#[test]
fn unchanged_external_context_preserves_local_display_history() {
    use crate::user_model::history::{Diff, DiffType, QueueDiffs};
    with_advancing_time(1_700_000_000_000, 1000, || {
        let mut raw = new_empty_model();
        raw._set("A1", "1");
        raw._set("B1", "=NOW()");
        raw.evaluate();
        let initial = raw.get_cell_value_by_index(0, 1, 2).unwrap();
        let mut model = UserModel::from_model(raw);
        model.set_user_input(0, 1, 1, "2").unwrap();
        model.pause_evaluation();
        let report = model.model.evaluation_report().clone();
        let reads = clock_reads();
        let queue = bitcode::encode(&vec![QueueDiffs {
            r#type: DiffType::Redo,
            list: vec![Diff::SetTimezone {
                old_value: "UTC".into(),
                new_value: "UTC".into(),
            }],
        }]);
        model.apply_external_diffs(&queue).unwrap();
        assert_eq!(*model.model.evaluation_report(), report);
        assert_eq!(clock_reads(), reads);
        model.undo().unwrap();
        assert_eq!(
            model.model.get_cell_value_by_index(0, 1, 2).unwrap(),
            initial
        );
    });
}

#[test]
fn remote_manual_links_preserve_unaffected_formula_cache_history() {
    let mut raw = new_empty_model();
    raw._set("A1", "1");
    raw._set("B1", "=A1+1");
    raw.evaluate();
    let mut remote =
        UserModel::from_model(crate::Model::from_bytes(&raw.to_bytes(), "en").unwrap());
    let mut model = UserModel::from_model(raw);
    model.set_user_input(0, 1, 1, "2").unwrap();
    let link = crate::types::Link::External {
        target: "https://manual.example".into(),
        tooltip: None,
    };
    remote.set_cell_link(0, 1, 2, link.clone(), None).unwrap();
    model
        .apply_external_diffs(&remote.flush_send_queue())
        .unwrap();
    model.pause_evaluation();
    model.undo().unwrap();
    assert_eq!(value(&model, 1, 2), "2");
    assert_eq!(model.get_cell_link(0, 1, 2).unwrap(), Some(link));
}

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
            let queue = model.flush_send_queue();
            assert!(model
                .paste_from_clipboard(0, cp.range, &cp.data, true)
                .is_err());
            assert_eq!(model.to_bytes(), bytes);
            assert_eq!(*model.model.evaluation_report(), report);
            assert_eq!(clock_reads(), reads);
            assert_eq!(model.get_selected_view(), selected);
            assert_eq!(value(&model, 5, 3), "5");
            assert!(model.flush_send_queue().len() < queue.len());
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
    for size in [100, 10_000] {
        let mut raw = new_empty_model();
        for row in 1..=size {
            raw.update_cell_with_number(0, row, 10, 7.0).unwrap();
        }
        raw._set("A1", "=SEQUENCE(5)");
        raw.evaluate();
        let mut model = UserModel::from_model(raw);
        model.pause_evaluation();
        for row in 1..=10 {
            model.set_user_input(0, row, 2, "1").unwrap();
            assert_eq!(model.model.evaluation.display.journal_positions, 1);
        }
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

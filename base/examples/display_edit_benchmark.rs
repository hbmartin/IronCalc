//! Synthetic display-history scaling benchmark; no timing assertions.
//! Run: cargo run --release -p ironcalc_base --example display_edit_benchmark
#![allow(clippy::unwrap_used, clippy::print_stdout)]
use ironcalc_base::{ClipboardData, Model, UserModel};
use std::time::Instant;

fn populated(rows: i32) -> UserModel<'static> {
    let mut raw = Model::new_empty("display benchmark", "en", "UTC", "en").unwrap();
    for row in 1..=rows {
        raw.update_cell_with_number(0, row, 10, 1.0).unwrap();
    }
    let mut model = UserModel::from_model(raw);
    model.pause_evaluation();
    model
}
fn main() {
    for rows in [1_000, 10_000, 50_000] {
        let mut model = populated(rows);
        let start = Instant::now();
        for index in 0..100 {
            model.set_user_input(0, 1, 2, &index.to_string()).unwrap();
        }
        println!(
            "100 paused edits; {rows} unrelated cells: {:?}",
            start.elapsed()
        );
        let start = Instant::now();
        for row in 1..=rows {
            model.set_user_input(0, row, 1, "1").unwrap();
        }
        println!("paused population; {rows} inputs: {:?}", start.elapsed());
    }
    for count in [100, 1_000, 5_000] {
        let mut model = populated(50_000);
        for row in 1..=count {
            model.set_user_input(0, row, 1, "1").unwrap();
        }
        model.set_selected_cell(1, 1).unwrap();
        model.set_selected_range(1, 1, count, 1).unwrap();
        let clipboard = serde_json::to_value(model.copy_to_clipboard().unwrap()).unwrap();
        let data: ClipboardData = serde_json::from_value(clipboard["data"].clone()).unwrap();
        let range = serde_json::from_value(clipboard["range"].clone()).unwrap();
        model.set_selected_cell(1, 3).unwrap();
        let start = Instant::now();
        model.paste_from_clipboard(0, range, &data, true).unwrap();
        println!(
            "paused cut; {count} cells, 50,000 unrelated cells: {:?}",
            start.elapsed()
        );
    }
}

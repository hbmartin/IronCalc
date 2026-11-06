//! Synthetic release benchmark; no workbook fixtures or wall-clock assertions.
//! Run: cargo run --release -p ironcalc_base --example calculation_benchmark
#![allow(clippy::unwrap_used, clippy::print_stdout)]
use ironcalc_base::Model;
use std::time::Instant;

fn model(kind: &str, rows: i32) -> Model<'static> {
    let mut m = Model::new_empty("benchmark", "en", "UTC", "en").unwrap();
    for row in 1..=rows {
        m.set_user_input(0, row, 1, "1".into()).unwrap();
        let formula = if kind.starts_with("running totals") {
            format!("=SUM(A$1:A{row})")
        } else {
            format!("=A{row}*2")
        };
        m.set_user_input(0, row, 2, formula).unwrap();
    }
    match kind {
        "scalar LET" => m.set_user_input(0, 1, 4, "=LET(x,A1,x*2)".into()).unwrap(),
        "real spill" => m.set_user_input(0, 1, 4, "=SEQUENCE(2)".into()).unwrap(),
        "small cycle" => {
            m.set_user_input(0, 1, 4, "=D1/2+10".into()).unwrap();
            m.set_iterative_calculation(true, 100, 1e-9).unwrap();
        }
        _ => {}
    }
    m.evaluate_headless();
    assert!(m.evaluation_report().converged);
    m
}
fn measure(base: &Model, incremental: bool, edits: i32, edited_row: i32) -> (f64, usize) {
    let mut times = Vec::new();
    let mut fallback = 0;
    for _ in 0..5 {
        let mut m = base.fork_evaluation_session().unwrap();
        if incremental {
            m.evaluate_incremental_headless(&[]);
        }
        let start = Instant::now();
        let mut n = 0;
        for edit in 0..edits {
            m.update_cell_with_number(0, edited_row, 1, (edit + 2) as f64)
                .unwrap();
            if incremental {
                n += usize::from(
                    m.evaluate_incremental_headless(&[(0, edited_row, 1)])
                        .is_none(),
                );
            } else {
                m.evaluate_headless();
            }
            assert!(m.evaluation_report().converged);
        }
        times.push(start.elapsed().as_secs_f64() * 1000.0);
        fallback = n;
    }
    times.sort_by(f64::total_cmp);
    (times[2], fallback)
}
fn main() {
    println!("scenario,formulas,edits,full_ms,incremental_ms,fallbacks");
    for (kind, rows, edits) in [
        ("scalar", 20000, 10),
        ("scalar LET", 20000, 10),
        ("real spill", 20000, 10),
        ("running totals", 2000, 3),
        ("running totals late", 2000, 3),
        ("small cycle", 20000, 3),
    ] {
        let base = model(kind, rows);
        let edited_row = if kind.ends_with("late") { rows } else { 1 };
        let (full, _) = measure(&base, false, edits, edited_row);
        let (incremental, fallbacks) = measure(&base, true, edits, edited_row);
        println!("{kind},{rows},{edits},{full:.3},{incremental:.3},{fallbacks}");
    }
}

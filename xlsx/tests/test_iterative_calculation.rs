#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use std::io::Cursor;

use ironcalc::export::save_xlsx_to_writer;
use ironcalc::import::{load_from_xlsx, load_from_xlsx_bytes};
use ironcalc_base::cell::CellValue;
use ironcalc_base::types::IterativeCalculation;
use ironcalc_base::Model;

fn round_trip(model: &Model) -> Model<'static> {
    let cursor = save_xlsx_to_writer(model, Cursor::new(Vec::new())).unwrap();
    let workbook = load_from_xlsx_bytes(&cursor.into_inner(), "model", "en", "UTC").unwrap();
    Model::from_workbook(workbook, "en").unwrap()
}

fn number(model: &Model, cell: &str) -> f64 {
    match model.get_cell_value_by_ref(cell).unwrap() {
        CellValue::Number(n) => n,
        other => panic!("expected a number in {cell}, got {other:?}"),
    }
}

#[test]
fn settings_survive_saving_and_loading() {
    let mut model = Model::new_empty("model", "en", "UTC", "en").unwrap();
    model.set_iterative_calculation(true, 50, 0.0001).unwrap();
    let loaded = round_trip(&model);
    assert_eq!(
        loaded.get_iterative_calculation(),
        IterativeCalculation {
            enabled: true,
            maximum_iterations: 50,
            maximum_change: 0.0001,
        }
    );
}

#[test]
fn default_settings_survive_saving_and_loading() {
    let model = Model::new_empty("model", "en", "UTC", "en").unwrap();
    let loaded = round_trip(&model);
    assert_eq!(
        loaded.get_iterative_calculation(),
        IterativeCalculation::default()
    );
}

#[test]
fn a_file_without_iteration_settings_loads_with_the_defaults() {
    let model = load_from_xlsx("tests/example.xlsx", "en", "UTC", "en").unwrap();
    assert_eq!(
        model.get_iterative_calculation(),
        IterativeCalculation::default()
    );
}

#[test]
fn a_saved_cycle_settles_after_loading() {
    let mut model = Model::new_empty("model", "en", "UTC", "en").unwrap();
    model
        .set_user_input(0, 1, 1, "=B1/2+10".to_string())
        .unwrap();
    model.set_user_input(0, 1, 2, "=A1+5".to_string()).unwrap();
    model.set_iterative_calculation(true, 100, 1e-9).unwrap();
    let mut loaded = round_trip(&model);
    loaded.evaluate();
    assert!((number(&loaded, "Sheet1!A1") - 25.0).abs() < 1e-6);
    assert!((number(&loaded, "Sheet1!B1") - 30.0).abs() < 1e-6);
}

fn with_calc_pr(attributes: &str) -> Vec<u8> {
    use std::io::{Read, Write};
    let model = Model::new_empty("model", "en", "UTC", "en").unwrap();
    let original = save_xlsx_to_writer(&model, Cursor::new(Vec::new()))
        .unwrap()
        .into_inner();
    let mut archive = zip::ZipArchive::new(Cursor::new(original)).unwrap();
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).unwrap();
        let name = file.name().unwrap().into_owned();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        if name == "xl/workbook.xml" {
            let mut xml = String::from_utf8(bytes).unwrap();
            let start = xml.find("<calcPr").unwrap();
            let end = start + xml[start..].find("/>").unwrap() + 2;
            xml.replace_range(start..end, &format!("<calcPr {attributes}/>"));
            bytes = xml.into_bytes();
        }
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(&bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[test]
fn excel_limits_and_zero_tolerance_round_trip() {
    for enabled in [false, true] {
        for count in [20_000, 32_767] {
            let mut model = Model::new_empty("model", "en", "UTC", "en").unwrap();
            model
                .set_iterative_calculation(enabled, count, 0.0)
                .unwrap();
            let loaded = round_trip(&model);
            assert_eq!(
                loaded.get_iterative_calculation(),
                model.get_iterative_calculation()
            );
        }
    }
}

#[test]
fn disabled_invalid_fields_default_independently() {
    for (attrs, count, delta) in [
        (
            "iterate=\"0\" iterateCount=\"-1\" iterateDelta=\"0\"",
            100,
            0.0,
        ),
        (
            "iterate=\"0\" iterateCount=\"abc\" iterateDelta=\"0.5\"",
            100,
            0.5,
        ),
        ("iterate=\"0\" iterateCount=\"32768\"", 100, 0.001),
        (
            "iterate=\"0\" iterateCount=\"32767\" iterateDelta=\"NaN\"",
            32767,
            0.001,
        ),
        ("iterateCount=\"20000\" iterateDelta=\"-1\"", 20000, 0.001),
        (
            "iterate=\"0\" iterateCount=\"0\" iterateDelta=\"abc\"",
            100,
            0.001,
        ),
    ] {
        let wb = load_from_xlsx_bytes(&with_calc_pr(attrs), "model", "en", "UTC").unwrap();
        assert_eq!(
            wb.settings.iterative_calculation,
            IterativeCalculation {
                enabled: false,
                maximum_iterations: count,
                maximum_change: delta
            }
        );
        let model = Model::from_workbook(wb, "en").unwrap();
        assert_eq!(
            round_trip(&model).get_iterative_calculation(),
            model.get_iterative_calculation()
        );
    }
}

#[test]
fn enabled_invalid_settings_are_not_silently_changed() {
    for attrs in [
        "iterateCount=\"-1\"",
        "iterateCount=\"0\"",
        "iterateCount=\"32768\"",
        "iterateCount=\"abc\"",
        "iterateDelta=\"-1\"",
        "iterateDelta=\"NaN\"",
        "iterateDelta=\"inf\"",
        "iterateDelta=\"abc\"",
    ] {
        let bytes = with_calc_pr(&format!("iterate=\"1\" {attrs}"));
        assert!(
            load_from_xlsx_bytes(&bytes, "model", "en", "UTC").is_err(),
            "{attrs}"
        );
    }
}

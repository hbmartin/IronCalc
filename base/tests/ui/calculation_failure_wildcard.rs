use ironcalc_base::types::CalculationFailure;
fn main() {
    let description = match CalculationFailure::NotEvaluated {
        CalculationFailure::NotEvaluated => "not evaluated",
        _ => "calculation failed",
    };
    assert_eq!(description, "not evaluated");
}

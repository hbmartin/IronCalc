use ironcalc_base::types::CalculationFailure;
fn main() {
    match CalculationFailure::NotEvaluated {
        CalculationFailure::InvalidSettings
        | CalculationFailure::CircularReference
        | CalculationFailure::IterationLimit
        | CalculationFailure::NonNumericCycle
        | CalculationFailure::NonFiniteValue
        | CalculationFailure::CyclicArray
        | CalculationFailure::RecursionLimit
        | CalculationFailure::VolatileCycle
        | CalculationFailure::NotEvaluated => (),
    }
}

#[test]
#[cfg(not(target_family = "wasm"))]
fn calculation_failure_requires_an_external_wildcard() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/ui/calculation_failure_wildcard.rs");
    cases.compile_fail("tests/ui/calculation_failure_exhaustive.rs");
}

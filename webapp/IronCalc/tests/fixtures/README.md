`invalid-iterations.ic` is an otherwise empty workbook created by Rust
`Model::new_empty("settings", "en", "UTC", "en")`, then assigned
`workbook.settings.iterative_calculation.enabled = true` and
`maximum_iterations = 0` through the public workbook before `to_bytes()`.
It exercises permissive loading and the real WASM context-setter error contract.
The Rust follow-up test validates the fixture settings and its encoded round trip.

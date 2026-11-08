`invalid-iterations.ic` is an otherwise empty workbook created by Rust
`Model::new_empty("settings", "en", "UTC", "en")`, then assigned
`workbook.settings.iterative_calculation.enabled = true` and
`maximum_iterations = 0` through the public workbook before `to_bytes()`.
It exercises permissive loading and the real WASM context-setter error contract.
The Rust follow-up test validates the fixture settings and its encoded round trip.

The base crate owns this fixture. Rust and the webapp's real-WASM tests read the
same bytes. To regenerate it after an intentional workbook-format change, run
this recipe in a small Rust program using `ironcalc_base`:

```rust
let mut model = ironcalc_base::Model::new_empty("settings", "en", "UTC", "en").unwrap();
model.workbook.settings.iterative_calculation.enabled = true;
model.workbook.settings.iterative_calculation.maximum_iterations = 0;
std::fs::write("base/tests/fixtures/invalid-iterations.ic", model.to_bytes()).unwrap();
```

`bash scripts/check-base-package.sh` verifies inclusion and compiles the packaged
library tests without any neighboring webapp source.

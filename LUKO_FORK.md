# Calculation fork

Base: upstream `eb9ca75bd99160c17902359d2d87225c4571c468` (0.9.1).

Iterative settings and the repeated-evaluation approach adapt the unmerged
[upstream PR #1466](https://github.com/ironcalc/IronCalc/pull/1466), by
SahilRajputdotexe. Upstream has not approved these semantics.

## Calculation contract

The fork preserves disabled/100/0.001 defaults and XLSX `calcPr` properties.
Iteration counts range from 1 to 32,767. Tolerances must be finite and nonnegative;
zero requires exact numeric equality, and positive tolerances require a strictly
smaller numeric change. Every iterative calculation starts from zero.

Circular calculations require at least two completed passes by default, so the
first pass establishes a baseline. `maximum_iterations` remains the total pass
budget: a cycle with a one-pass budget reports `IterationLimit`, while acyclic
workbooks still succeed in one pass. Every current cycle participant needs a
comparison value from the preceding completed pass.

Use `set_require_two_iterative_passes(false)` to restore first-pass comparison
with zero; `get_require_two_iterative_passes()` reads the option. Rust and Python
use these names, while JavaScript uses `setRequireTwoIterativePasses` and
`getRequireTwoIterativePasses`. The switch is local runtime configuration: forks
inherit it, structural changes and history retain it, and loading a workbook
resets it to true. It is absent from workbook serialization, undo history, and
collaboration diffs. Raw Model changes invalidate only the report without
calculating, even when iteration is disabled. Formula caches, dependency indexes,
hyperlinks, formatting caches and their display timestamp remain available until
recalculation. An invalidated report still prevents forking a session. UserModel
changes calculate once unless paused. Setting the same value is a complete no-op.

Convergence means the configured change threshold was met, not that a
mathematical limit has been established. For `A1=A1+0.0001` and tolerance `0.001`,
the default mode succeeds on pass two (value `0.0002`), and legacy comparison
succeeds on pass one (value `0.0001`). A tighter or zero tolerance makes that
example exhaust its pass budget. The first-pass residual remains the change
from the zero seed, even when that pass cannot establish convergence.

Scalar numeric and Boolean cycles are supported, including 1×1 dynamic anchors
produced by defined names, INDEX, LET, INDIRECT and LAMBDA. Boolean values must
match exactly, independently of tolerance; numeric/Boolean transitions require
another pass. Text cycles, genuine multi-cell spills, blocked multi-cell spills,
and CSE array cycles remain unsupported. The dynamic identity of scalar anchors
is preserved so later edits can still make them spill.

Consumers must inspect `Model::evaluation_report()` and reject unsuccessful
reports; reaching the iteration cap is not convergence. `residual` is the largest
numeric delta, with a minimum of 1 for Boolean/type changes. An incremental call
following an unsuccessful report performs full evaluation before reporting a new
result. Ordinary spreadsheet errors remain cell values; a successful report is
not a guarantee that every cell is error-free.

Newly created, loaded and invalidated models report `converged=false`,
`iterations=0`, and `failure=Some(CalculationFailure::NotEvaluated)`. A successful
calculation has `failure=None`. `NotEvaluated` is appended to the public Rust
enum and the enum is non-exhaustive: downstream matches must include a wildcard
arm to allow future failure kinds. Reports are runtime state and do not change
workbook or collaboration encodings.

Only executed RAND, RANDBETWEEN and RANDARRAY calls feeding a runtime cycle cause
`VolatileCycle`. Random values are not frozen. Unrelated or downstream-only
volatile formulas, inactive branches, unused named functions and deleted
formulas do not block convergence. Runtime dependency tracking includes cached
reads, ranges and spill anchors; abandoned passes do not contribute dependencies
or failure diagnostics.

TODAY and NOW use one timestamp captured at the outermost calculation entry.
Every iterative pass, spill restart, cell replay, incremental fallback and
conditional-format evaluation belonging to that calculation shares the same
instant, converted using each function's requested timezone. Date-dependent
cycles can therefore converge. Later full or incremental calculations capture a
fresh timestamp; forks begin without an active timestamp. This is per calculation,
not per simulation run. Native and browser implementations preserve the existing
second precision. TODAY and NOW remain volatile for incremental invalidation.

A separate runtime display timestamp survives after the active clock scope
closes. Standalone conditional-formatting refreshes, including theme changes,
reuse that timestamp without reading the clock. Completed formula calculations
replace it even when convergence fails; invalid settings before any pass preserve
the previous timestamp. Full and incremental evaluation still rebuild formatting
for the current rules and cell locations at that retained instant, including after
structural edits and undo/redo. Headless calculation owns the active clock scope
and updates the display timestamp without rebuilding formatting; the subsequent
formatting refresh reuses that timestamp after the calculation scope closes.
Forks copy the display timestamp but start without an active clock.

Loading remains on demand: its initial formatting evaluation captures a fresh
snapshot shared by any formulas it reads, without calculating the whole workbook
or marking its report successful. Later redraws reuse that snapshot until an
explicit full or incremental calculation. Neither active nor display timestamps
are serialized, so reloading does not reproduce the saved calculation instant.
Native and browser conversions floor negative timestamps to the preceding second.

XLSX import preserves valid iteration settings, including zero tolerance. When
iteration is disabled, malformed/out-of-range fields individually default to
100 iterations or 0.001 tolerance. Invalid enabled settings reject import;
there is no silent clamping. API setters validate settings even when disabled.

## Incremental calculation and sessions

Incremental invalidation adapts the AST dependency extraction from unmerged
[upstream PR #1435](https://github.com/ironcalc/IronCalc/pull/1435), by svettypoo.
Rectangle ranges are deduplicated and indexed by immutable per-column interval
trees. Each matched rectangle expands its dependents once per recalculation.
Wide ranges retain a separate containment path. Dense dependency graphs can
still require quadratic work.

Incremental evaluation supports scalar formulas and 1×1 dynamic anchors.
Iterations, genuine arrays, blocked spills, spill changes and unsuccessful
baselines use full headless evaluation. Array eligibility is cached before
index construction, avoiding repeated index builds during fallback. Removing
arrays allows incremental evaluation again after the next full calculation.
Pass every edited cell to incremental calls. Structural model APIs invalidate
cached state automatically; direct mutations of public workbook internals still
require explicit full evaluation.

`fork_evaluation_session()` shares immutable parsed formulas and dependency
indexes while isolating values and scratch state. It preserves dynamic
hyperlinks and all resource limits. The baseline must have completed a
successful evaluation before it is forked.

`set_max_lambda_depth()` / `get_max_lambda_depth()` configure the native LAMBDA
call-depth ceiling (default 256, minimum 1). The existing evaluation stack-byte
budget also applies and may stop recursion sooner. Cell chains unwind/replay;
a LAMBDA that cannot fit within a single cell's budget returns `#NUM!` and
`RecursionLimit`. Raising the depth ceiling alone does not increase the byte
budget. Size both limits for the worker's actual stack and build profile.
Defaults remain 256 LAMBDA calls and a 256 KiB evaluation stack budget. Stored
`#NUM!` errors identify either the configured call-depth limit or the configured
stack-byte budget; the report retains `RecursionLimit` for both.

Changing the effective LAMBDA-depth ceiling or stack-byte budget resets only the
calculation report to `NotEvaluated`, retaining result caches and display time.
These raw Rust setters do not recalculate; reevaluate before forking a session.
Unchanged effective values are no-ops, including LAMBDA depths clamped to one.
The cell-evaluation-depth setting only controls unwind/replay, so changing it
retains the report and results.

An explicit native configuration tested with `FACTORIAL(70)` is:

```rust
let worker = std::thread::Builder::new()
    .stack_size(32 * 1024 * 1024)
    .spawn(|| {
        let mut model = ironcalc_base::Model::new_empty("model", "en", "UTC", "en")
            .unwrap();
        model.set_max_evaluation_stack(8 * 1024 * 1024);
        // Keep the default 256-call LAMBDA ceiling.
        model.new_defined_name(
            "FACTORIAL", None, "LAMBDA(n,IF(n=0,1,n*FACTORIAL(n-1)))"
        ).unwrap();
        model.set_user_input(0, 1, 1, "=FACTORIAL(70)".into()).unwrap();
        model.evaluate_headless();
        assert!(model.evaluation_report().converged);
    }).unwrap();
worker.join().unwrap();
```

This does not increase engine defaults or application worker stacks. Resource
limit configuration remains a Rust API; binding additions cover the comparison
switch only.

The public Model iteration setter still evaluates immediately. UserModel
setting changes and undo/redo respect paused evaluation and evaluate at most
once per diff batch. Resuming evaluation retains the existing behavior: it
re-enables automatic evaluation without itself triggering a calculation.

## Compatibility and validation

The internal bitcode workbook format changed with calculation settings.
Use XLSX as the interchange format; do not assume old `.icalc` files load.
`WorkbookSettings` implements `PartialEq`, but not `Eq`, because its tolerance
is a floating-point value.

`SetIterativeCalculation` is appended to the collaboration diff enum, preserving
pre-existing upstream variant positions. This changes the earlier fork's
mid-enum encoding: upgrade communicating clients together. There is no decoder
for earlier fork queue payloads and no mixed-version compatibility guarantee.

The toolchain and calculation CI use Rust 1.99.1. Synthetic regressions cover
report correctness, resource limits, scalar/Boolean cycles, volatility,
structural changes, spill transitions, serialization and import normalization.
Run the reproducible performance sample with:

```sh
cargo run --release -p ironcalc_base --example calculation_benchmark --locked
```

It reports medians of five runs, with index warmup outside the timer. Timings
are informational, not CI thresholds. Test-only counters check index reuse and
cycle-only snapshots deterministically.

Release measurements on macOS arm64, Rust 1.99.1, November 6, 2026, comparing
`226b7d4` with these fixes. Times below are milliseconds for the entire edit
batch, medians of five runs; both revisions use the same benchmark source.

| Scenario | Formulas | Edits | Full before / after | Incremental before / after | Fallbacks before / after |
| --- | ---: | ---: | ---: | ---: | ---: |
| Sparse scalar edits | 20,000 | 10 | 16.254 / 19.085 | 0.004 / 0.008 | 0 / 0 |
| One scalar LET anchor | 20,000 + 1 | 10 | 16.224 / 18.491 | 42.002 / 0.011 | 10 / 0 |
| Genuine spill | 20,000 + 1 | 10 | 16.224 / 17.499 | 40.836 / 17.434 | 10 / 10 |
| Running totals, first input | 2,000 | 3 | 43.873 / 41.394 | 44.129 / 41.334 | 0 / 0 |
| Running totals, last input | 2,000 | 3 | 42.853 / 40.688 | 0.047 / 0.042 | 0 / 0 |
| One cycle amid scalar formulas | 20,000 + 1 | 3 | 240.531 / 219.877 | 242.316 / 218.594 | 3 / 3 |

Scalar anchors now retain incremental execution; genuine arrays avoid wasted
index builds before falling back. Editing the first running-total input still
affects every formula and requires dense range evaluation. The additional
eligibility checks add full-evaluation overhead in the sparse scalar sample;
these measurements are not a claim that every workload becomes faster.

Only generic calculation changes and synthetic tests belong here. Application
code and proprietary financial workbook fixtures stay in the private backend.

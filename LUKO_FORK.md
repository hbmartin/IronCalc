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

Only executed time/random calls feeding a runtime cycle cause `VolatileCycle`.
Unrelated or downstream-only volatile formulas, inactive branches, unused named
functions and deleted formulas do not block convergence. RAND, RANDBETWEEN,
RANDARRAY, NOW and TODAY are not frozen between passes. Runtime dependency
tracking includes cached reads, ranges and spill anchors; abandoned passes do
not contribute dependencies or failure diagnostics.

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

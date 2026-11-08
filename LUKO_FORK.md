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

Conditional formatting reads stored cell results. References, ranges, spill
references, names and INDIRECT never calculate workbook cells during a redraw;
formatting expressions cannot attach dynamic hyperlinks or change the calculation
report. Rule expressions themselves retain their normal function behavior.

A separate runtime display timestamp survives after the active clock scope
closes. Standalone formatting refreshes, including theme changes, reuse it without
reading the clock. Completed formula calculations replace it even when
convergence fails. Invalid settings before any pass preserve an existing display
timestamp; if none exists, initial formatting establishes one. Full and
incremental evaluation rebuild formatting for current rules and cell locations
at that retained instant. Headless calculation owns the active scope; formatting
runs after it closes. Forks copy display time but begin without an active clock.

Loading is permissive and formats saved results without evaluating formulas.
Saved formula and spill values are retained, missing results remain unevaluated,
and the report stays `NotEvaluated` until explicit calculation. Initial formatting
captures a fresh snapshot; later redraws reuse it. Timestamps and dynamic links
are not serialized, so time-based rules can differ from saved date values until
calculation, and formula-generated links are recreated by calculation. Manual
links remain workbook metadata. Native and browser conversions floor negative
timestamps to the preceding second.

Structural edits invalidate reports, dependency indexes and calculation scratch
without discarding displayed results. Cached formula values and dynamic links
follow their owning cells through row, column, cut and sheet operations, even
when calculation fails or is paused. Formatting refreshes once at the outer edit
or history boundary, using retained values. Intact spill groups move as groups;
a partially broken dynamic spill loses its children and resets its extent while
retaining a surviving anchor's cached value until calculation. Partial CSE edits
remain restricted; complete cuts preserve array identity. These are retained results, not a successful new calculation.

Local undo/redo keeps sparse runtime display deltas alongside existing history,
restoring cached values, spills and dynamic links without calculation. The deltas
are not serialized or sent to collaborators. A runtime cell-storage journal
captures each touched position before its first write and remains active through
the final calculation, including spill restarts and failed convergence. Routine
edits inspect only these positions when building history, rather than copying
the workbook before and after every edit. Structural remapping retains its
broader snapshots. Journals do not affect encoded bytes or equality, and clones,
loads and forks start without active observation.

Structural remapping and cell movement mark only positions whose ownership moves
or disappears, plus their destinations and affected spill groups. Identity
restoration skips unchanged payloads. Unrelated equal caches are not retained
in structural history. Replay-sensitive display entries are retained even when the cached payload at a position
compares equal: undo/redo first moves cells, so position equality alone cannot
identify redundant history. Ordinary edits still omit equal payloads. Merge
operations transfer cached formula results and dynamic links to their anchor;
paused autofill marks calculation out of date without running a pass.

Display supplements share immutable storage across undo/redo. Remote conflict
pruning uses copy-on-write, preserving exact volatile results without copying
their vectors each time an entry changes stacks. History remains unbounded.
Edit guards own their mutable borrow through final calculation and publication.
If an edit panics, unwinding releases nesting, pending effects and observers and
marks calculation out of date. This cleanup does not roll back partial mutations
or run calculation/formatting. Conditional-formatting rebuilds publish only a
complete overlay; unwinding retains the last complete overlay and schedules
repair at the next successful edit or explicit refresh. Ordinary validation
errors retain their existing atomicity guarantees.

Calculation and formatting requests are separate. Rule changes refresh formatting
without invalidating reports or dependencies, including during local history and
external replay. Width, height, ordinary style, gridline and frozen-pane edits do
not reevaluate rules. Theme changes refresh theme-dependent overlays. Relevant
nested edits refresh formatting once at the outermost boundary, and explicit
calculation still refreshes formatting even when settings are invalid.

External diffs preserve available state but cannot restore runtime metadata
absent from their payload; calculation recreates it. Explicit remote writes
retire conflicting display supplements from both local history stacks, including
spill owners/children and dynamic links; unrelated supplements and ordinary undo
entries survive. Same-text formula replacement still changes ownership.
Structural remote edits retire supplements on affected sheets and sheets with
rewritten formulas; global calculation-context and defined-name changes retire
all display supplements. This does not rebase encoded collaborative history.
Deleted or replaced formulas lose their associated runtime links.

Clipboard paste validates its source/destination, CSE restrictions, merge
footprint, finite inputs and formula/name/formatting rewrites before entering a
mutating edit scope. Rejected pastes preserve saved bytes, cached values, spills,
links, formatting, reports, dependency state, clocks, selection, history and
queues. Successful paste publishes its history after the prepared operations and
selection are applied. Invalid translated merges remain skipped. Nonpositive
row/column moves and zero displacements are no-ops before display remapping.
Partial-spill clearing finds owners through affected spill cells, without a
full-sheet scan. `display_edit_benchmark` measures paused population and large
cuts in release mode; CI uses deterministic touched-position/owner-visit counts
rather than timing thresholds.

Changed timezone/locale setters reject enabled-invalid iteration settings before
mutation; UserModel also rejects these changes while paused. Rejection leaves
context, results, display state, history and queues untouched, including through
undo/redo and external batches. Unchanged settings are no-ops. A successful
unpaused context change calculates once, including calculations that complete
with nonconvergence. Resuming automatic evaluation does not itself calculate.

Flushing outgoing diffs is not an acknowledgement of delivery. Callers retain
the bytes until the receiver applies them successfully. A rejected incoming
context batch applies nothing and is not queued internally; retry the same bytes
after resuming evaluation or repairing settings, preserving delivery order.
The regional-settings drawer validates selections before invoking setters and
keeps the drawer and selections open with an inline error after rejection.
Unchanged selections skip setters, and language-only changes remain available
when evaluation is paused.

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
Scalar range clears remember populated positions and include them automatically
in the next incremental call, even when its explicit edit list is empty. Repeated
clears accumulate; cleared formulas participate in dependency-index rebuilding.
Pass every other edited cell to incremental calls. Structural model APIs invalidate
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

### Clipboard preparation and display ownership

Clipboard operations are prepared on isolated state containing only affected
cells, complete touched spill groups, and required formula rewrites. Ordinary
setters provide input and permission validation; preparation never evaluates
formulas or reads the clock. Metadata needed to resolve formulas/styles is
copied, but unrelated cell storage is not. Unit inference shares the ordinary
setter implementation and reads affected styles from the stage, with untouched
styles read from the full original model. Inferred styles are explicit diffs.
Every observed cell or link write is checked against the prepared footprint
before publication. Workbook-wide metadata copies and a broader read-through
staging redesign remain deferred; preparation is not constant-cost in workbook
metadata size. Commit publishes the prepared cells,
metadata and existing diffs without repeating fallible input validation. Rejected
preparation changes no live values, reports, caches, selection, history or queues.
Translated merges determine which formulas survive; invalid translated merges
continue to be skipped. Destination contents are captured before clearing and
sent as grouped clears before writes, preserving nonempty destinations through
undo/redo and allowing peers to replace existing CSE groups. Complete CSE cuts
and dependent CSE rewrites retain their
array kind and extent through grouped clears and existing array diffs, including
1x1 CSE formulas moved by merging. Partial CSE edits remain rejected. A moved
dynamic merge anchor retains its value but starts with a 1x1 cached extent.
Cross-sheet cuts retain references to cells outside the cut on their source
sheet (for example, `=C5` becomes `=Sheet1!C5` after moving to Sheet2).

Remote display conflicts are classified exhaustively, accumulated in application
order and pruned once per batch from both history stacks. Empty footprints skip
history traversal; unaffected shared supplements retain their handles. Merge
geometry, old/new spill owners and newly created or restored sheet identities
retire stale ownership, including reused sheet IDs. Ordinary encoded history
still is not rebased. Missing remote runtime results are not synthesized.

Scalar clears mark reports NotEvaluated and block forks while retaining an
internal baseline and accumulated clear positions eligible for incremental
evaluation. Report invalidation retires this baseline centrally. Structural, spill/array,
settings/resource-limit and failed-calculation invalidation remain conservative.
Neutral and no-op edits do not carry pending formatting effects into later edits;
recovery from an earlier panic is the exception that repairs formatting.
Raw Model conditional-formatting mutations rebuild overlays without formula
calculation or report invalidation. Actual priority swaps refresh; boundary
no-ops do not. Failed live edits reconcile links and pending formatting from
stored results without calculating; partial mutations retain existing error
semantics. Isolated failed clipboard preparation still changes no live state.
Rewritten formulas and their array children/links retain equal cache supplements
when replay would recreate them. Unrelated equal values remain filtered.

Regional settings keep separate setter history and calculations. All selectable
values are validated before the first setter. If an unexpected later setter
fails, applied changes remain saved and redraw, while the drawer retains pending
selections and displays a translated error. No combined settings API is added.
Known rejections use exact canonical error strings; unexpected errors use the
translated fallback. Formula-language IDs come from the engine generator JSON
at build time, without bundling its formula translations or adding a binding API.
Locale/timezone transition preflight rejects a whole incoming batch while paused
or when enabled iteration settings are invalid at that transition; iteration
setting changes alone remain allowed while paused. The Python retry example
catches WorkbookError and lets unrelated failures propagate.

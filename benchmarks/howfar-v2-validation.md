# how-far redesign validation — 2026-10-03

Historical snapshot of the initial `codex/howfar-v2` implementation and its
API-hardening follow-up, based on callback branch `0adafe1`. The later
Result-preserving API and extraction of diagnostics into `how-far-really` are
recorded in [the current validation report](howfar-results-validation.md).
The `profile` / `diagnostics` tracker features below belonged to this snapshot.
The original enough and callback checkouts are untouched. This is a library
implementation plus an isolated, reproducible adoption fixture; it does not
change or publish the production encoder.

## Design decision

Keep `&dyn Pulse` at library boundaries and erase adapter implementation types.
The core contains the protocol, owners, sharing, stages and pacing; the application
crate owns the accounting tree, callbacks, observation and optional profiling.
There is one accounting implementation. A callback can inspect progress lazily
at a check; counting never dispatches it.

`share()` preserves planning, cancellation and denominator revisions in owned
tasks. Shared views do not own completion. Owners finish after joining workers;
dropping an owner abandons its phase even if views survive. Administration
already in flight defers abandonment until it exits. `NotRun` prevents aborted
pipelines from crediting their unexecuted tails; explicit skipping still earns
its planned weight. `Paced::finish()` checks the last partial batch.

This changes the unpublished callback branch API: `FnPulse` moves from core to
`how-far-along/callback`, takes a name and a lazy `Checkpoint`, and no longer calls
user code from reporting or destruction. Legacy `handle()` remains available
with its original count/check-only contract. New code should prefer `share()`.

## Compile-time seams

| Dependency/features | Included | Excluded |
| --- | --- | --- |
| `how-far`, no features | `no_std + alloc` protocol, NoPulse, owners, stages, pacing | Tracker, callbacks, clocks, runtime, serialization |
| core `adapters` | Erased `WithStop`, including cancellation-only construction | Tracking |
| `how-far-along`, no defaults | Native counters, tree, summaries, observers, pollers | std, JSON, checkpoint callbacks, profiling |
| `callback` | FnPulse and lazy Checkpoint; works without std | Clocks, thread pool, bindings |
| `json` | Snapshot JSON formatting | std requirement |
| defaults: `std,json` | Standard mutex metadata backend and JSON | Callback and profiling |
| `profile` / `diagnostics` | Explicit timing / analysis | Nothing silently substituted into ordinary pulses |
| encoder `progress` off | Original algorithm and Stop API | Dependency and all added reporting sites |

Core adds no dependency beyond enough. `SharedPulse` adds an Arc allocation per
new owned view and an atomic refcount increment per clone; borrow within hot
loops. Planning allocates child owners and metadata. A summary walks the tree
without allocating; snapshots allocate copies. Neither should be requested at
pixel frequency. An enabled callback still adds checkpoint dispatch even if it
does not inspect the tree.

## Cold compilation

The [raw samples](howfar-v2-cold.json) contain three fresh-target builds per
configuration and profile. `CARGO_INCREMENTAL=0`, offline, with package and OS
file caches warm. These are artifact-cold builds, not a reboot/download benchmark.
Linux x86_64, Ryzen 9 5900XT, rustc 1.99.0. `perf instructions:u` includes Cargo
and dependency compiler processes. Elapsed medians are machine/load-dependent.
Source fingerprints identify the crates used at measurement time; later
documentation wording does not change the measured implementation.

| Configuration | Check / debug / release seconds | Check / debug / release million instructions |
| --- | ---: | ---: |
| baseline-core | 0.187 / 0.218 / 0.266 | 399.7 / 685.5 / 907.5 |
| core | 0.180 / 0.194 / 0.229 | 361.8 / 545.5 / 691.8 |
| core-adapters | 0.184 / 0.211 / 0.265 | 386.7 / 645.8 / 928.7 |
| tree-minimal | 0.252 / 0.314 / 0.443 | 682.7 / 1,426.4 / 2,597.9 |
| tree-default | 0.266 / 0.327 / 0.435 | 742.4 / 1,464.0 / 2,969.0 |
| callback | 0.272 / 0.325 / 0.435 | 724.7 / 1,469.6 / 3,021.3 |
| diagnostics | 0.342 / 0.475 / 0.664 | 1,176.6 / 2,847.8 / 6,845.4 |
| encoder-baseline | 5.603 / 6.644 / 8.455 | 59,223.1 / 88,928.1 / 207,088.5 |
| encoder-feature-off | 5.632 / 6.687 / 8.454 | 59,223.7 / 88,944.0 / 207,109.7 |
| encoder-feature-on | 5.658 / 6.700 / 8.507 | 59,482.2 / 89,449.6 / 208,032.1 |

Default core requires 23.8% fewer release-build instructions than the callback branch, including Cargo/dependencies.
Enabling encoder progress adds 0.46% release-build instructions to its full library graph; feature-off differs by about 0.01%.


The encoder comparison builds the actual library dependency graph, not a fake
codec loop. Its progress dependency uses core only. Applications choosing tree
tracking additionally pay the tree build cost shown above. Callback and default
tree rows are different feature sets: the callback row has std and callback but
no JSON, so it is not an isolated subtraction for callback cost.

The separate `dev/bench-how-far-build.py` guard isolates rustc work, rejects new
core dependencies/build scripts/features beyond adapters, and caps unoptimized
IR growth per Stages call site and per tracker build. See
[its measured output](howfar-v2-build.txt). Keeping the erased cancellation
adapter behind a feature avoids paying its code generation cost in every codec.

## Actual encoder adoption

`dev/adopt-zenpng.py` archives zenpng revision
`27393eef995aea329cfbc1df5cac5e3f7346b4cb` into pristine and modified fixtures.
The proof adds `encode_rgb8_with_pulse`, one optional pulse in existing options,
four existing compression-stage wrappers, and counting at verified strategy
success boundaries. Deep filtering/compression checks use the stage's Stop
interface. The encoder's original error type and original entry point remain.

The fixture changes five Rust files, adding 196 lines and removing four before
formatting, plus dependency/feature declarations. Most additions are stage
wrapping, feature gates and initialization of existing option literals; there
are no cloned compression algorithms and no progress generics in codec loops.
Weights 10/60/20/10 are illustrative, not calibrated ETA claims. Only screening
has measured units; the other stages remain unknown-total. Other pixel formats,
APNG, and every compression effort are not fully adopted by this proof.

Tests verify byte parity for both serial and parallel encoder configurations,
positive actual strategy counts, cancellation after real encoding work, NotRun
tails, and that enabling progress preserves the old cancellation API.

Runtime results use the actual Fast encoder, generated RGB images, three process
runs per case and byte-parity assertions on every encode. Executed instructions
include startup and one reference encode; elapsed time excludes that reference.
The callback does nothing; an application callback's work is additional.

| Mode | Extra instructions, 64×64 (100 encodes) | Extra instructions, 512×512 (20 encodes) |
| --- | ---: | ---: |
| Existing API with progress feature enabled | 0.0153% | 0.0003% |
| New API with NoPulse | 0.0143% | 0.0008% |
| Tree | 0.1527% | 0.0077% |
| No-op checkpoint callback | 0.1656% | 0.0122% |

Baseline elapsed totals were 46.0 ms and 355.9 ms. Timing changes were mostly
noise, so this does not establish a throughput improvement. Results apply to
these coarse stage/report boundaries and these inputs, not per-pixel calls or
all encoders. [Raw runtime samples](howfar-v2-encoder-runtime.json).

## Threading and relaxed atomics

Reports retain relaxed saturating atomic updates. Counts are observation data,
not a publication fence for image buffers. Joins synchronize worker results;
acquire/release publishes lifecycle transitions and frozen terminal state. There
is no feature to weaken these orderings. Relaxed read-modify-write still incurs
cache-line ownership transfers and CAS retries.

The [contention stress test](howfar-v2-contention.csv) performs 250,000 units per
worker with 1/2/4/8 workers. Every sample asserts exact final counts. At eight
workers, one shared leaf took 62.93 ms reporting each unit, 0.74 ms batching 64,
and 0.25 ms batching 1024. Separate leaves took 0.91 ms with direct reporting.
This deliberately isolates counter contention and is not encoder throughput.
Choose batching first; give workers different children only when separate
totals/outcomes are meaningful. Local buffers can lag by a partial batch until
flushed; explicitly finish them before joining/finishing their phase.

Native tests also exercise owned nested library calls, revisions, zero counts,
overflow, late views, owner-drop/administration races, concurrent first-stop
latching, reentrant observation without locks across callbacks, and diagnostic
sharing without transferring span completion rights. Miri checks metadata and
owner-drop races with strict provenance; it is not an exhaustive scheduler proof.

## Wasm and no_std + alloc

Both minimal tracking and `callback,adapters,json` cross-compile for
`wasm32-unknown-unknown` and `thumbv7em-none-eabihf`. Native effective feature
matrix: 24 combinations. Core MSRV 1.86 and tracker MSRV 1.88 are tested.

Pointer atomics are required by Arc. Targets without native 64-bit atomics use
native-width saturation and expose `counter_max`/overflow; callback support
does not require 64-bit atomics. Pointer-atomic-free targets are unsupported.
A critical-section-backed u64/portable ownership backend is a possible separate
opt-in extension, not silently inserted into the report path in this change.
The no_std host must supply a critical section covering all participating cores;
local interrupt masking alone is insufficient for shared multicore progress.
The no_std try-observation API also enters that provider's critical section.

The browser fixture uses full shared pulses and worker-local Paced buffers in a
four-worker Rayon phase. Six Chromium/WebKit tests cover serial/parallel/serial
execution, UI progress, shared-memory cancellation while workers are busy,
stable terminal counts, profiling, and JSPI/chunked event-loop boundaries.
The raw Node Wasm probe checks ordinary timers do not run during synchronous
work, explicit JSPI suspension/resumption/cancellation, and worker progress.

Bindings, Rayon, nightly build-std, shared-memory setup and COOP/COEP remain in
the host fixture. They are not library dependencies. Callbacks do not themselves
yield. UI updates belong in a host observer/poller; synchronous worker checks
must read shared cancellation, not wait for a worker message handler. Browser
tests use Playwright WebKit, not packaged Safari, and the Wasm workload models
encoder scheduling rather than porting zenpng itself. Worker termination and
panic abort cannot run Drop cleanup.

## Reproduction

### API hardening follow-up

After the initial redesign, `run_stoppable` was restricted to `StopReason`, the
private Stages lifecycle became Ready/Running/Stopped, and owners/reporting
guards gained `must_use` guidance. Four compile-fail examples cover the error
boundary, single completion ownership, non-owning shared views, and discarded
pacing guards. Existing failure, cancellation, skipping and caught-panic tests
exercise the runner transitions.

The local zenresize source uses `run_stoppable` for resampling, sharpening and
blur; those operations return StopReason already. That checkout is pinned to an
older how-far API named Steps: this is a source audit, not a build or migration
of zenresize against the redesign. The cross-crate pipeline fixture had two
calls annotated with PipelineError; these now use StopReason and an explicit
conversion at the enclosing API boundary. The real zenpng fixture uses
run_classified and required no migration.

The follow-up passes workspace tests, all four compile-fail cases, Clippy,
MSRV tests, public API regeneration, the real encoder fixture and the release
checkpoint harness. [The repeated build guard](howfar-hardening-build.txt)
still measures 64 IR lines per Stages call and unchanged tracker IR. Core rustc
instructions are 202.0 / 369.5 / 512.2 million for check/debug/release versus
198.0 / 363.6 / 504.0 in the initial redesign. The full cold-build and runtime
tables above retain their original measurement scope; they were not rerun for
this API hardening.

### Initial redesign validation

Completed locally: workspace tests (615 passed), no-default tracker tests (54),
core 1.86 tests (48), tracker 1.88 tests (103), all-target/all-feature Clippy,
rustdoc, public API snapshots, formatting, feature matrix and cross-target
checks, three real encoder adoption tests, six browser tests, and the raw Wasm
probe. Miri passed all five library unit tests plus metadata publication,
owner-drop/administration, and concurrent callback stop-latching tests. Cold
compilation and IR budgets pass; the checkpoint-cost harness builds in release.
These are local results; remote CI has not been run for this branch.

From the implementation worktree:

```sh
cargo test --workspace --all-features
cargo test -p how-far-along --no-default-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo +1.86 test -p how-far --all-features
cargo +1.88 test -p how-far-along --all-features
cargo test --manifest-path apidoc/Cargo.toml
MIRIFLAGS=-Zmiri-strict-provenance cargo +nightly miri test -p how-far-along --lib
MIRIFLAGS=-Zmiri-strict-provenance cargo +nightly miri test -p how-far-along --all-features --test redesign root_abandonment_racing

python3 dev/adopt-zenpng.py /path/to/zenpng /tmp/howfar-encoder-adoption
cargo test --manifest-path /tmp/howfar-encoder-adoption/probe/Cargo.toml --features progress --offline
python3 dev/bench-how-far-cold.py --baseline /path/to/enough--howfar-callback --encoder /tmp/howfar-encoder-adoption --runs 3 --output benchmarks/howfar-v2-cold.json
python3 dev/bench-how-far-build.py --runs 3
python3 dev/bench-encoder-runtime.py /tmp/howfar-encoder-adoption --output benchmarks/howfar-v2-encoder-runtime.json
cargo run --release -p how-far-along --example contention -- 250000
```

Run performance commands sequentially. The fixture uses the encoder checkout's
lockfile and locally available dependency sources; offline reproduction needs
those dependencies cached. Browser setup and commands are in
[the browser README](../dev/how-far-browser/README.md). CI retains cross-target,
feature-powerset, build-budget, MSRV and browser coverage.

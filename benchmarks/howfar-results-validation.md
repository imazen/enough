# Result handoffs, diagnostics extraction, and stop composition

Validated in `codex/howfar-v2` on 2026-10-04. This report extends the
[earlier implementation and browser results](howfar-v2-validation.md).
The original enough checkout, callback baseline, and zenpng checkout were not
modified. This work is local; no publication or merge was performed.

## Behavior and coverage

- Ordinary `Stages`/`Phases` preserve `Result<T,E>` and the identity of non-Clone
  errors. Automatic borrowed stop classification and explicit foreign-wrapper
  classifiers both work. Root and child result handoffs are consuming.
- States are NotStarted, Running, Finished(Outcome); result-aware helpers infer
  Skipped/NotRun only for untouched work. Started unfinished work is Abandoned.
  Successful recovered parents retain failed child attempts. Terminal parent
  observations cannot be rewritten by retained orphaned owners.
- Three separate example crates demonstrate a no_std codec, no_std nested
  pipeline and instrumented application. They cover success, recovery, fatal
  failure, cancellation, forgotten handoffs/reports, swallowed/misclassified
  cancellation, repeated stage use, rejected planning, late/branch reports and
  missing final paced checks. They assert that diagnostics never change results.
- WithStop combines or replaces owned/borrowed policies. Tests check nested
  descendants, completion forwarding, scoped threads, spawned owned views,
  stop precedence and NotShareable for borrowed policies. The application tests
  a caller-owned stage with an additional encoder-local borrowed stop source.
- Profiling/diagnostics moved to `how-far-really`; no diagnostic types are
  re-exported by the tracker and no core/tracker types are re-exported by really.
  Public API snapshots and negative import doctests enforce the boundaries.
- Workspace all-feature tests passed (634 tests/doctests at the full-workspace
  run), followed by focused tests for the final edits. Clippy with warnings
  denied, formatting and diff checks passed. All 25 selected feature-power-set
  checks passed. Core passed Rust 1.86; tracker, diagnostics and examples passed
  Rust 1.88. Core and tracker also passed their no-default-features tests.
- no_std + alloc library builds passed for thumbv7em-none-eabihf and
  wasm32-unknown-unknown, including optional core adapters, tracker callbacks/JSON
  and the nested example libraries. These are compile validations, not embedded
  hardware runtime tests. A correct multicore critical-section provider remains
  the no_std application's responsibility.
- Six Chromium/WebKit tests passed: host yields, shared-memory Rayon completion,
  UI observation and cancellation while a worker is busy. The raw Wasm Node
  host-boundary probe also passed. The browser uses the extracted diagnostics
  crate explicitly. The fixture required its pinned wasm-bindgen 0.2.123 CLI;
  the system CLI was older. WebKit here is not Apple's packaged Safari.
- Eleven example trace JSON documents, including incidents, parsed successfully
  with Python's JSON parser. Each trace and embedded progress document uses
  schema 2. Tests check bounded retention and source locations, including reports
  made through legacy handles after owner completion.

## Compilation

Measurements use rustc 1.99.0 on this host, three samples and median values,
independent of the MSRV checks. Artifact-cold means a fresh target directory,
CARGO_INCREMENTAL=0 and offline Cargo; package and OS caches remain warm. Perf
counts include Cargo and dependency compilers. These numbers are not promises
for other machines or toolchains.

| Build | Check (s) | Debug (s) | Release (s) |
| --- | ---: | ---: | ---: |
| Callback baseline core | 0.187 | 0.218 | 0.263 |
| Core (default) | 0.182 | 0.231 | 0.318 |
| Core + checked | 0.194 | 0.245 | 0.334 |
| Core + adapters | 0.189 | 0.238 | 0.319 |
| Tracker, no defaults | 0.261 | 0.333 | 0.461 |
| Tracker, std + JSON | 0.276 | 0.347 | 0.466 |
| Tracker, std + callbacks | 0.282 | 0.349 | 0.464 |
| Diagnostics (tracker included) | 0.392 | 0.516 | 0.750 |
| zenpng baseline | 5.591 | 6.648 | 8.439 |
| zenpng progress feature off | 5.608 | 6.692 | 8.454 |
| zenpng progress feature on | 5.620 | 6.706 | 8.480 |

Encoder feature-on compiler-instruction overhead versus baseline: check **0.48%**, debug **0.82%**, release **0.70%**.

[Raw cold samples](howfar-results-cold.json) include compiler instructions and
source fingerprints. [Per-crate build measurements](howfar-results-build.txt)
separately measure rustc instructions and unoptimized LLVM IR. Ordinary
Stages::run contributes **79 IR lines per call site**, under the existing 120-line
budget. Bookkeeping is compiled once instead of duplicated per closure. Tracker
IR is under its 22,000-line ceiling; the separate diagnostic crate is under
35,000. On Rust 1.99 the measured values are 18,657 and 31,694. The old combined
tracker+diagnostics ceiling was 55,000.

The [Rust 1.88 guard probe](howfar-results-build-msrv.txt) measured **88** IR lines
per stage call and 20,336/33,345 tracker/diagnostic lines. This is why the tracker
ceiling accommodates 22,000 rather than the initial 20,000 proposal. That probe
uses one instruction-count sample, not the three-sample medians above. Its
measurement command supplied 22,000/40,000 ceilings; the measured values also
satisfy the final default 22,000/35,000 ceilings. Both core 1.86 and tracker/really
1.88 test runs were additionally verified with explicit compiler-bin paths,
bypassing this host's mise shims.

The optional `checked` feature isolates TryStages/RunError; `adapters` isolates
stop composition. Tracker `std`, `json`, `callback`, and `adapters` are additive.
No core or tracker dependency introduces the profiling clock, diagnostic analysis,
Rayon, wasm-bindgen, or an executor. Diagnostics itself requires std and JSON.

## Real encoder adoption

The fixture pins zenpng commit `27393eef995aea329cfbc1df5cac5e3f7346b4cb` and
materializes independent base/progress source trees. The new result API removes
progress-specific error conversion helpers. `whereat::At<PngError>` is a foreign
wrapper, so its stages and final handoff use explicit borrowed classifiers.
Original PNG errors, including stopped errors, reach the caller unchanged.

The patch adds 185 Rust lines and removes 7 across 5 files, plus the optional
manifest dependency/feature. Much of this is cfg routing between the original
entry points and the progress-aware entry point, and carrying one optional pulse
through existing options. It reuses codec loops and counts existing verified
strategy results. It does not migrate every zenpng API, validate all codecs, or
claim an application-wide speedup.

Three adoption tests pass: serial/parallel byte-identical PNG output with tracked
strategy counts, callback cancellation after real work, and preservation of the
original cancellation API when progress is enabled. Cold costs for baseline,
feature-off and feature-on encoder builds are in the table and raw samples.

The previously audited real zenresize adopter has three `run_stoppable` uses for
resize, sharpen and blur, returning StopReason. That remains the strict legacy
helper under `checked`; normal new Result-based adoption uses `Stages::run`.
The zenresize checkout was not migrated in this change.

## Interpretation limits

An inferred Skipped phase does not prove its work was optional. A successful
parent with a Failed child does not prove the fallback was correct. Cancellation
checks made outside an instrumented policy can be absent from its trace. A
WithStop replacement bypasses the original check, including callbacks; combining
preserves it. Borrowed policies support scoped workers but cannot become static
handles. `share`/`try_handle` return NotShareable; legacy infallible `handle`
panics on that unsupported conversion rather than silently losing cancellation.

Drop counts a final paced batch but cannot return a stop error or observe a
plain Result's variant. Destructors do not run on process abort/worker termination.
Relaxed counters remain approximate telemetry and do not synchronize encoded
buffers; join workers before completing owners. No new stronger ordering or
critical-section counter fallback was introduced by this change.

## Reproduce

```sh
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo hack check --feature-powerset --no-dev-deps -p how-far -p how-far-along -p how-far-really
cargo +1.86 test -p how-far --all-features
cargo +1.88 test -p how-far-along --all-features -p how-far-really -p how-far-example-app
cargo build -p how-far -p how-far-along -p how-far-example-pipeline --no-default-features --features how-far/adapters,how-far-along/callback,how-far-along/json --target thumbv7em-none-eabihf
# Repeat the previous build with --target wasm32-unknown-unknown.
cargo test --manifest-path apidoc/Cargo.toml
cargo run -p how-far-example-app -- --json
python3 dev/adopt-zenpng.py /path/to/zenpng /tmp/howfar-result-adoption
cargo test --manifest-path /tmp/howfar-result-adoption/probe/Cargo.toml --features progress
python3 dev/bench-how-far-build.py --runs 3
python3 dev/bench-how-far-cold.py --baseline /path/to/enough--howfar-callback --encoder /tmp/howfar-result-adoption --runs 3 --output /tmp/cold.json
HOW_FAR_BINDGEN=/path/to/0.2.123/wasm-bindgen bash dev/how-far-browser/build.sh
cargo build --manifest-path dev/how-far-wasm/Cargo.toml --target wasm32-unknown-unknown
node dev/how-far-wasm/check.mjs
npm test --prefix dev/how-far-browser
```

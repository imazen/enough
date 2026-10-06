# how-far validation

What is tested, where, and what the tests do not cover. See
[the design notes](how-far-design.md) for why things work the way they do.

## Test matrix

| Scenario | Where |
| --- | --- |
| Counting through every sink shape; original call sites survive forwarding | `crates/how-far/tests/interface.rs` |
| The `Pulse` contract on `NoPulse`: nested plans, validation, shared views, `split_array`, outcomes from results; `PhaseSpec::validate_split` and `Child::inert` for implementors | `crates/how-far/tests/pulse.rs` |
| `FnPulse`: checkpoint-only callbacks, lazy observation, reentrant observation, final paced cancellation, concurrent first-stop latching, no callback from Drop; shares accounting with the tree tests | `crates/how-far-along/tests/redesign.rs` |
| References, boxes and `Arc`s of a pulse: generic code taking `&NoPulse` by value, an owned `Box<dyn Pulse>` defaulting to it, stops, reports, splits and shares forwarded | `crates/how-far/tests/forwarding.rs` |
| `Paced`: never reaching a no-op pulse, exact counts per interval, stops seen at the next reach, pending units counted after an early return, saturation, call sites | `crates/how-far/tests/paced.rs` |
| A hand-written `Pulse` shared by scoped threads; `'static` threads through shared views | `crates/how-far/tests/pulse_threads.rs` |
| `StopOnly`: an application's token reaching every nested phase, owned tokens shared with spawned work, borrowed tokens refusing to share | `crates/how-far/tests/stop_only.rs` |
| Result handoffs: foreign error identity, `complete_with` and its classified twin keeping an early `?` inside the handoff | `crates/how-far/tests/results.rs` |
| Shared views administering concurrently: no refused revisions or plans, no report lost to a racing split, bounded revision history | `crates/how-far-along/tests/sharing.rs` |
| `Stages` libraries calling each other: success, nested stop, failure versus cancellation, stage finish errors, abandonment | `crates/how-far/tests/composition.rs`, `crates/how-far-along/tests/pulse.rs` |
| Sizes of everything a caller holds or passes, asserted at compile time | `crates/how-far/tests/footprint.rs`, `crates/how-far-along/tests/footprint.rs` |
| A library crate (codec) and a library that calls it (pipeline), driven by an application, all in separate crates | `examples/how-far-app/tests/cross_crate.rs` |
| Scoped fork-join children, spawned `'static` threads, cancellation crossing threads, an observer on another thread, a tree moved into a thread | `examples/how-far-app/tests/threads.rs` |
| Rayon: a stage shared by 1–8 workers, nested parallelism, recursive `join`, `scope`, `'static` `spawn`, the global pool | `examples/how-far-app/tests/rayon_pools.rs` |
| `'static` ownership: codec contexts that own their stop, `Arc` and `Box` pulses, Tokio `spawn_blocking` with async cancellation, async tasks reporting | `examples/how-far-app/tests/statics.rs` |
| Diagnostics across crates, including checks inside a codec context credited to the right stage | `examples/how-far-app/tests/diagnose.rs` |
| Application-planned trees: serial → 30% parallel → serial, repeated joins, Rayon and manual threads sharing a counter, totals and revisions, overrun and overflow, every outcome, frozen and abandoned records, a report racing with the finish | `crates/how-far-along/tests/phases.rs`, `crates/how-far-along/src/tree.rs` |
| A codec-style pipeline with two parallel waves, a terminal renderer, and a Tokio request whose client disconnects | `crates/how-far-really/tests/hosts.rs` |
| Pollers: thread-affine callbacks, lazy shared snapshots, busy and recursive dispatch, panics, posted delivery, workers stopped by a callback | `crates/how-far-along/tests/polling.rs` |
| Profiling: per-task gaps, call-site counts, overlap and stragglers, cancellation latency, bounded retention, clock faults, report timing on and off, workers sharing a span | `crates/how-far-really/tests/profiling.rs` |
| Diagnostics: report gaps versus stop gaps, covering spans, stage-weight candidates, negligible stages, callbacks | `crates/how-far-really/tests/diagnostics.rs` |
| Metadata replacement concurrent with snapshots, under Miri with strict provenance | `crates/how-far-along/src/sync.rs`, `crates/how-far-along/tests/phases.rs` |
| A real Wasm timer boundary, JSPI suspension and cancellation, progress posted from a worker | `dev/how-far-wasm/check.mjs` |
| A UI thread observing and cancelling a `wasm-bindgen-rayon` pool in Chromium and WebKit | `dev/how-far-browser/browser.spec.mjs` |
| `enough` on Rust 1.85 and the how-far crates on 1.88; `no_std` builds for Cortex-M (including optional callbacks with native-width counters) and wasm32; every feature combination; i686, aarch64 Linux and Windows, Intel macOS | CI |

No test suite proves every consumer's behavior. There is no built-in ETA
model or executor; exported observations support them without claiming that
durations are CPU time or that fractions are elapsed time.

Browser and Wasm guidance is in [the design notes](how-far-design.md#wasm).

## Build and runtime cost

`dev/bench-how-far-build.py`, run in CI on Rust 1.88, permits only the additive
core `adapters` and `checked` features, only the `enough` production dependency,
and no build script. It caps `Stages::run` at 120 unoptimized LLVM IR lines per
call site, tracker code at 22,000 and the `how-far-really` diagnostics crate at
35,000. It also counts rustc's instructions, which unlike wall time do not
depend on machine load. `dev/how-far-checkpoint-cost` counts, with perf, what
each checkpoint style and a three-stage plan cost around the same
`#[inline(never)]` defilter.

[The 2026-10-06 record](../benchmarks/how-far-2026-10-06.md) has both at commit `2bcc73a`: 79 / 18,382 / 29,703
IR lines on Rust 1.99 and 88 / 19,986 / 31,228 on 1.88; a `step` into a live
tree at 41 to 77 extra instructions per checkpoint depending on its stop and
sink, a `Paced` step at about 2, and no checkpoint code at all in a loop
stepping into `&NoPulse`. It also records the zenpng adoption tests.

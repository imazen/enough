# how-far-along [![CI](https://img.shields.io/github/actions/workflow/status/imazen/enough/ci.yml?style=flat-square&label=CI)](https://github.com/imazen/enough/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/how-far-along?style=flat-square)](https://crates.io/crates/how-far-along) [![lib.rs](https://img.shields.io/crates/v/how-far-along?style=flat-square&label=lib.rs&color=blue)](https://lib.rs/crates/how-far-along) [![docs.rs](https://img.shields.io/docsrs/how-far-along?style=flat-square)](https://docs.rs/how-far-along) [![MSRV](https://img.shields.io/badge/MSRV-1.88-blue?style=flat-square)](https://doc.rust-lang.org/cargo/reference/manifest.html#the-rust-version-field) [![license](https://img.shields.io/crates/l/how-far-along?style=flat-square)](#license)

Track, observe, and profile work that reports through
[`how-far`](https://github.com/imazen/enough/blob/main/crates/how-far/README.md).

Libraries depend on `how-far` and accept `&dyn Pulse`. Applications and
tests depend on this crate and pass a `PulseTree`. The tree records what the
library declares (phases, weights, counts, outcomes), and an `Observer` reads
it from any thread. Reading never blocks the workers' counting.

```toml
[dependencies]
how-far-along = "0.1"
# Optional, usually as dev-dependencies:
#   features = ["profile"]      # time checks and reports per task
#   features = ["diagnostics"]  # turn traces into source-located advice
```

Everything in `how-far` is re-exported, so an application needs only this
crate.

## Run a library and watch it

```rust
use almost_enough::Stopper;
use how_far_along::{Outcome, Phase, PhaseSpec, ProgressExt, Pulse, PulseTree, Stages, Total};

/// Library code. It depends only on `how-far`.
fn encode(rows: u64, pulse: &dyn Pulse) -> Result<(), how_far::RunError<how_far::StopReason>> {
    let mut stages = Stages::new(pulse, &[
        PhaseSpec::new("analyze", 1, Total::Exact(rows)).units("rows"),
        PhaseSpec::new("encode", 3, Total::Exact(rows)).units("rows"),
    ])?;
    for _ in 0..2 {
        stages.run_stoppable(|stage| (0..rows).try_for_each(|_| stage.step(1)))?;
    }
    stages.finish()?;
    Ok(())
}

// Application code.
let stop = Stopper::new(); // from almost-enough; stop.cancel() from any thread stops the work
let tree = PulseTree::new(Phase::new("encode", Total::Unknown), stop.clone());
let observer = tree.observer();   // cheap to clone; hand it to a UI thread
let result = encode(100, &tree);
tree.finish(Outcome::from_result(&result, |_| true))?;

let done = observer.snapshot();
assert_eq!(done.fraction(), Some(1.0));
assert_eq!(done.children[1].units, "rows");
# Ok::<(), how_far_along::PlanError>(())
```

The tree owns its stop policy, so it is `'static`: move it into a worker
thread and keep the observer. The application finishes the root when the
library returns; dropping it unfinished records `Abandoned`.
[`Outcome::from_result`](https://docs.rs/how-far/latest/how_far/enum.Outcome.html#method.from_result)
turns a library's result into an outcome.

## Read progress

`Observer::snapshot()` returns an owned tree of `Snapshot`s: names, weights,
units, totals and their revisions, counts, statuses and outcomes.

- `fraction()` is the weighted share of counted work, from 0 to 1, or `None`
  while a denominator is still unknown. It is not elapsed time and not an
  ETA: a nearly finished fork-join can still wait on one slow branch.
- `unresolved_fraction()` is the share of the plan with no usable total yet,
  so a display can show "known work plus an unknown remainder" honestly.
- Counting to 100% does not finish a phase. Only an outcome does, and
  `Succeeded` or `Skipped` counts the phase as done even with zero work.
- Exceeding an `Exact` total and saturating a counter stay visible.

On a UI thread, use `Observer::try_snapshot()`. It returns `None` instead of
waiting if another thread is replacing a phase's metadata at that instant;
try again on the next frame. Reporting never takes a lock: a `Reporter`
updates a counter and two flags with atomics.

## Plan a tree yourself

Applications can plan phases directly with owned, `'static` handles:

```rust
use how_far_along::{Execution, Phase, PhaseSpec, Report, Total};

let mut job = Phase::new("encode", Total::Unknown);
let [mut prepare, mut tiles] = job.split(Execution::Sequence, [
    PhaseSpec::new("prepare", 1, Total::Exact(1)),
    PhaseSpec::new("tiles", 9, Total::Exact(64)).units("tiles"),
])?;
prepare.reporter().advance(1);
prepare.finish()?;
let reporter = tiles.reporter(); // clone it for every worker
std::thread::scope(|scope| {
    for _ in 0..4 {
        let reporter = reporter.clone();
        scope.spawn(move || (0..16).for_each(|_| reporter.advance(1)));
    }
});
tiles.finish()?;
job.finish()?;
assert_eq!(job.observer().snapshot().fraction(), Some(1.0));
# Ok::<(), how_far_along::PlanError>(())
```

A `Phase` owns its plan and outcome; a cloned `Reporter` can only count.
Wrap any phase, including a child, in `PulseTree::new(phase, stop)` to hand it
to a library as `&dyn Pulse`. `ext::ReportExt::batched` gives each worker a
private buffer that publishes in batches, for counters shared by many cores.

## Callbacks

The `poll` module runs callbacks over snapshots when you poll; it starts no
threads and reads no clocks.

- `LocalPoller` runs `FnMut` callbacks on the polling thread, so they can
  hold `Rc`, UI handles, and borrowed state. Poll from your event loop or
  timer, and once more after the work joins to show the final state.
- `SharedPoller` runs `Send + Sync` callbacks on whichever worker polls
  first. A worker that finds a dispatch already running returns at once.

A snapshot is built only if a callback asks for one, and at most once per
dispatch. To let a callback stop the work, give it a clone of the stop
policy, such as an `almost_enough::Stopper`.

## Measure checkpoint cadence

With the `profile` feature, a `Profiler` records spans: one per task, chunk,
or attempt. `span.instrument(stop_or_sink)` wraps a stop policy or progress
sink and records every check and report at its original call site. A trace
reports the longest stretch without a check, call rates per site, time spent
inside checks, cancellation latency from request to observation and to
return, and how parallel tasks overlapped. Times are wall time, not CPU time.
Traces export as JSON and as a text table.

With the `diagnostics` feature, `DiagnosticPulse` wraps the `PulseTree` you
pass to a library and gives every phase the library plans its own span.
`Trace::diagnose` then points at source lines: long stretches without a
check, coarse reporting units, hot call sites, slow callbacks, and stage
weights that differ from measured time. See the
[testing and tuning guide](https://github.com/imazen/enough/blob/main/docs/how-far-testing-and-tuning.md).

## Features and platforms

| Build | What you get |
| --- | --- |
| default (`std`) | Trees, observers, and pollers; metadata behind short standard mutexes |
| `default-features = false` | The same API on `no_std + alloc`, through the application's [`critical-section`](https://docs.rs/critical-section) provider |
| `profile` | The span profiler; requires `std` |
| `diagnostics` | `DiagnosticPulse` and `Trace::diagnose`; implies `profile` |

Features only add items. No feature changes what another one records.

Counting and cancellation use atomics only. Metadata (plans, totals,
outcomes) lives in immutable `Arc`s; a short lock guards only the swap of
one `Arc` for another, and readers copy and walk the tree outside it. There
is no spinning fallback: a spinning lock cannot yield a browser's event loop.
Counts saturate at `u64::MAX` (at `usize::MAX` on targets without 64-bit
atomics) and record the overflow.

On the web, run CPU work in workers and read snapshots from the UI thread
with `try_snapshot`. The repository's
[browser fixture](https://github.com/imazen/enough/blob/main/dev/how-far-browser/README.md)
runs a `wasm-bindgen-rayon` pool in Chromium and WebKit, and its
[Wasm probe](https://github.com/imazen/enough/blob/main/dev/how-far-wasm/README.md)
shows progress callbacks crossing a JSPI suspension.

Neither crate uses `unsafe`. `how-far-along` requires Rust 1.88.

## License

Licensed under either of [MIT](https://github.com/imazen/enough/blob/main/LICENSE-MIT)
or [Apache-2.0](https://github.com/imazen/enough/blob/main/LICENSE-APACHE), at your option.

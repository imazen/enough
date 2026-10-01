# Test and tune a library that accepts `&dyn Pulse`

Keep the production dependency small:

```toml
[dependencies]
how-far = "0.1"

[dev-dependencies]
how-far-along = { version = "0.1", features = ["diagnostics"] }
```

First run an operation with `how_far::NoPulse` to check the no-observer path.
Then pass `PulseTree` from a test: it gives the library one `&dyn Pulse` while
an `Observer` lets the test assert child names, completed units, totals and
terminal outcomes. See [zenresize's focused test](https://github.com/imazen/zenresize/blob/codex/howfar-along-resizer/tests/how_far_progress.rs) for output parity, nesting, cancellation after exactly 12 rows, and skipped later stages.

To measure a library without changing its signature, wrap the caller's pulse:

```rust
use how_far_along::{Phase, PulseTree, Total, Unstoppable};
use how_far_along::diagnostics::{DiagnosticPulse, Options};
use how_far_along::profile::{Profiler, StdClock};

let profiler = Profiler::new(StdClock::new(), 512);
let stop = Unstoppable;
let pulse = PulseTree::new(Phase::new("encode", Total::Unknown), &stop);
let observer = pulse.observer();
let measured = DiagnosticPulse::new(&pulse, observer.clone(), &profiler);
// Replace this line with the real library call, which should finish the pulse:
// my_library::encode(input, &measured)?;
let trace = profiler.snapshot().with_progress(observer.snapshot());
for finding in trace.diagnose(&Options::default()) {
    eprintln!("{finding}");
}
```

`DiagnosticPulse` makes a span for each child phase. A stage of a sequential
split (`Steps`) is timed from when the previous stage finished, or from the plan
for the first stage, and gets a span even if it never checks or reports, so a
stage that works first and calls `step` afterwards is not measured as instant.
Other phases start at their first check or report; time before it is unknown.
Time between stages counts toward the next stage. For a phase shared by
several workers, the span is an aggregate: one busy worker can hide another
worker's long tail. Add a separate `profiler.span(node_id, "chunk",
SpanKind::Work)` and `span.instrument(stop_or_progress)` inside each logical
worker when that distinction matters. Finish spans after their tasks finish and
join workers before taking the final snapshot. Set the profiler capacity high
enough to retain every relevant span; incomplete coverage is reported.
`DiagnosticPulse` reports `may_stop()`/`may_report()` as `true` even over
`NoPulse`, so a library that skips checkpoints behind them still shows them.

## Is a slow gap a progress seam or missing cancellation checks?

They are separate measurements. A `ReportGap` says how long progress stayed
silent; it also says how many stop checks the same task made inside that
interval and the longest stop gap there. If checks are frequent, the finding
calls it a progress-granularity seam: a unit (a frame, a tile) is the smallest
thing the library reports, and more checks would not help. If no checks fall
inside, the same interval is a cancellation gap too, and a `StopGap` finding
names the line it ends at.

A library often holds its own `'static` stop token (for example an encoder
built with `with_stop`), so a borrowed `&dyn Pulse` never sees checks made deep
inside one unit. Then the stage looks sparse even though the encoder checks
often. Instrument that token from the **same profiler**:

```rust
let inner = profiler.span(node_id, "raw frame encode", SpanKind::Work);
let encoder = Encoder::new().with_stop(Adapter(inner.instrument(Unstoppable)));
// ... encode one frame, then:
inner.finish(Outcome::Succeeded);
```

A work span that ran across at least 90% of a gap and made checks is named in
the `StopGap`/`ReportGap` evidence with its checks and longest gap, and the
advice says cancellation there may already be covered. A separate `Profiler`
has its own clock epoch and cannot be correlated. `Adapter` bridges a `Stop`
trait from a different `enough` version to `how_far::Stop` in a few lines:
`fn check(&self) -> Result<(), E> { how_far::Stop::check(&self.0).map_err(..) }`.
`Stop::check` is `#[track_caller]`, so the adapter keeps the library's call
sites without an attribute of its own. Time before a library's first
checkpoint cannot be seen by an inner span either; cover opaque setup with a
span of its own.

The same profiler works for a library using only `enough::Stop`. In a test,
create a work span, pass `span.instrument(stop)` to the library, finish the
span, and diagnose the trace. No progress tree is required; the output covers
stop-call frequency and time between checks. The `enough` production crate
does not gain a feature or dependency.

The default diagnostic targets are **10 ms between stop checks**, **50 ms
between progress reports**, **10 ms per subscriber callback**, and **10 ms
between successive invocations of that subscriber**. All are
configurable on `Options`. The check and report rate hints have separate
thresholds; a high rate alone does not prove wasted work. Diagnostic mode
reads a clock on every report and twice per check, so compare instrumented
runs with ordinary runs before changing a hot loop.

To measure callback work, wrap the subscriber body:

```rust
// Inside a LocalPoller or SharedPoller callback:
// profiler.measure_callback(node_id, "render", || {
//     render(event.snapshot()); // Include lazy snapshot construction.
//     Control::Continue
// })
```

The findings include maximum and p95 callback time over retained invocations,
plus the longest start-to-start interval between invocations. A progress report
does not itself schedule a browser UI turn; visible smoothness depends on the
application's polling or posted-message cadence. A callback that never runs
has no measured interval, so instrument the caller's poll schedule separately
when diagnosing missing polls.

When a completed sequential plan has instrumented spans for every child,
diagnostics compares their wall times with the declared relative weights and
prints a `PhaseSpec::new(...)` sketch. Treat it as a candidate from this run.
A stage measured under `Options::negligible_stage_share` (2% by default) is too
small to calibrate from one run, and may be input-dependent (a flush that is
trivial for this input but not for another). It keeps its declared weight, the
other stages are rescaled around it, and it cannot trigger advice by itself.
A negligible stage that the plan gave more than half the bar is still
reported. Repeat across representative inputs, hardware and configurations
before changing the library's stable phase weights. Fork/join, incomplete
traces, failed stages and overlapping spans do not produce weight advice.

Run the focused checks with:

```sh
cargo test -p how-far-along --features diagnostics --test diagnostics
```

A [runnable example](../crates/how-far-along/examples/diagnostics.rs) prints
real findings without extra dependencies:

```sh
cargo run -p how-far-along --example diagnostics --features diagnostics
```

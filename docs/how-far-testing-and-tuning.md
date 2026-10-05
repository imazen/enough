# Test and tune a library that accepts `&dyn Pulse`

Keep the production dependency small and put the tracker in dev-dependencies:

```toml
[dependencies]
how-far = "0.1"

[dev-dependencies]
how-far-along = "0.1"
how-far-really = "0.1"
```

## Test what the library reports

Run each operation twice: once with `how_far::NoPulse` and once with a
`PulseTree`, and check that the output is identical. Then assert on the tree
the library left behind. Finish the root yourself; the library never does.

```rust
use how_far_along::{Outcome, Phase, PulseTree, Status, Total, Unstoppable};

let tree = PulseTree::new(Phase::new("encode", Total::Unknown), Unstoppable);
let observer = tree.observer();
// let result = my_library::encode(&input, &tree);
// assert_eq!(result, my_library::encode(&input, &how_far::NoPulse));
tree.finish(Outcome::Succeeded)?;
let root = observer.snapshot();
assert_eq!(root.status, Status::Finished(Outcome::Succeeded));
// Check stage names, units, totals, completed counts and outcomes:
// assert_eq!(root.children[0].name, "decode");
// assert_eq!(root.children[0].completed, rows);
# Ok::<(), how_far_along::PlanError>(())
```

Worth a test each:

- **Cancellation at an exact point.** Give the tree a stop policy that trips
  when a snapshot reaches a chosen count, and assert that the active stage
  is `Cancelled`, later stages are `NotRun`, and completed work up to the
  stop is still counted.
- **Failure versus cancellation.** Feed corrupt input and assert `Failed`,
  not `Cancelled`. `Stages::run_classified` needs a correct `is_stop`.
- **Calling another library.** Run the library inside a stage of an outer
  plan, the way an application's pipeline would, and assert both levels
  finish.
- **Parallel work.** Run under a Rayon pool of 1, 2, 4 and 8 threads, and
  assert that a stage shared by workers counts each unit once.

The repository's
[scenario tests](../tests/test-how-far-app/tests)
do all of these against a pretend codec and a pipeline built on it, across
crates, scoped and spawned threads, Rayon, and Tokio.

## Measure checkpoint cadence

`DiagnosticPulse` measures a library without changing its signature. Wrap the
tree, pass the wrapper, and finish the wrapper:

```rust
use how_far_along::{Outcome, Phase, PulseTree, Total, Unstoppable};
use how_far_really::diagnostics::{DiagnosticPulse, Options};
use how_far_really::profile::{Profiler, StdClock};

let profiler = Profiler::new(StdClock::new(), 512);
let tree = PulseTree::new(Phase::new("encode", Total::Unknown), Unstoppable);
let measured = DiagnosticPulse::new(tree, &profiler);
let observer = measured.observer();
// let result = my_library::encode(&input, &measured);
measured.finish(Outcome::Succeeded)?;
let trace = profiler.snapshot().with_progress(observer.snapshot());
for finding in trace.diagnose(&Options::default()) {
    eprintln!("{finding}");
}
# Ok::<(), how_far_along::PlanError>(())
```

Every phase the library plans gets its own span, tied to its node in the
tree. A stage of a sequential plan is timed from when the previous stage
finished, so work done before a stage's first checkpoint is measured, and a
stage that never checks at all still gets a span. Other phases start at their
first check or report.

Workers that share one phase share its span, so one busy worker can hide
another's long silence. When that matters, give each worker its own
`profiler.span(node, "chunk 3", SpanKind::Work)` and pass
`span.instrument(stop_or_sink)` to that worker. Finish spans and join workers
before taking the trace, and give the profiler enough capacity for every
span; the trace reports anything dropped or still running.

`DiagnosticPulse` returns `true` from `may_stop()` and `may_report()`, so a
library that skips checkpoints for no-op pulses still makes the calls being
measured.

### Checks inside code that owns its stop

Codecs often keep their stop policy in a context object: an encoder built
with `with_stop(impl Stop + 'static)` checks deep inside each frame. A
borrowed pulse cannot be stored there directly; an owned view from
`stage.share()` can, and under
`DiagnosticPulse` that handle is instrumented. Checks the encoder makes
through it count toward the stage that handed it out. Borrowed stop adapters
cannot become owned views; use scoped workers or an owned stop source.
Handle sharing failure explicitly: the sketch below assumes the enclosing
library error represents its chosen response to `NotShareable`, rather than
requiring every library error to convert all planning errors:

```rust,ignore
stages.run_classified(Error::is_stop, |stage| {
    let mut encoder = Encoder::new(config).with_stop(stage.share()?);
    for frame in frames {
        encoder.encode(frame)?;
        stage.step(1)?;
    }
    Ok(())
})?;
```

If the encoder's `Stop` trait comes from a different version of `enough`, a
four-line adapter bridges them. `Stop::check` is declared `#[track_caller]`,
so the adapter keeps the encoder's call sites without an attribute of its
own.

### A coarse report or missing checks?

A `ReportGap` finding says how long a task went without reporting, and also
how many cancellation checks it made inside that stretch and the longest gap
between them:

- **Frequent checks inside:** the finding calls it a progress-granularity
  seam. The library's smallest reportable unit, such as a frame, takes that
  long; more checks would not help. Report smaller units, split the stage, or
  accept the coarse bar.
- **No checks inside:** it is a cancellation gap too, and a `StopGap` finding
  names the line where the gap ended.

Checks made by another span on the same profiler count when that span ran
across at least 90% of the gap; the evidence names it. A separate `Profiler`
has its own clock epoch and cannot be correlated.

### Stage weights

When a sequential plan finished and every stage has a span, diagnostics
compares measured wall time with the declared weights and prints a
`PhaseSpec::new(...)` sketch. Treat it as one run's candidate: wall time
includes waits and changes with input, hardware and configuration. A stage
that took less than `Options::negligible_stage_share` (2% by default) is too
small to calibrate from one run (a flush that is trivial for this input may
not be for the next), so it keeps its declared weight and cannot trigger
advice on its own, unless the plan gave it more than half the bar. Fork-join
plans, failed stages, incomplete traces and overlapping spans get no weight
advice.

### Defaults and cost

The default targets are 10 ms between cancellation checks, 50 ms between
reports, 10 ms per callback, and 10 ms between runs of one callback. All are
fields of `Options`. A high call rate alone does not prove waste, so check
and report rates have separate thresholds.

Measuring costs time: an instrumented check reads the clock twice, and with
report timing on (which `DiagnosticPulse` enables) a report reads it once.
Compare against an uninstrumented run before changing a hot loop.

### Callbacks

Wrap a callback's body to measure it, including any snapshot it builds:

```rust,ignore
poller.subscribe(move |event| {
    profiler.measure_callback("render", || render(event.snapshot()));
});
```

Findings report the slowest and 95th-percentile callback time, and the
longest interval between two runs. A report does not schedule a UI update;
how smooth a display looks depends on how often the application polls.

## Run the examples

```sh
cargo test -p how-far-really --test diagnostics
cargo test -p how-far-example-app
cargo run -p how-far-really --example diagnostics
cargo run -p how-far-example-app
```

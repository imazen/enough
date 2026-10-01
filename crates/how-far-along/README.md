# how-far-along

Consumer-owned progress trees, callbacks, and opt-in execution profiling.

Library interfaces depend only on the small [`how-far`](../how-far/README.md)
crate. Applications and library tests opt into this crate. `PulseTree` supplies
the same `&dyn Pulse` a library accepts, including cancellation and nested
phases, while keeping tracking machinery out of the library dependency graph.
Neither crate permits unsafe code.

**MSRV: Rust 1.88.** `enough` and `almost-enough` retain Rust 1.85. The browser
test application's threaded Wasm build uses nightly; these libraries do not.

```toml
[dependencies]
how-far-along = "0.1"
# Optional profiling:
# how-far-along = { version = "0.1", features = ["profile"] }
# Dev-only call-frequency and smoothness advice:
# how-far-along = { version = "0.1", features = ["diagnostics"] }
# Embedded: always requires alloc and a platform critical-section provider.
# how-far-along = { version = "0.1", default-features = false }
```

## Give a library one Pulse

```rust
use how_far_along::{Phase, PulseTree, Total, Unstoppable};

let stop = Unstoppable;
let pulse = PulseTree::new(Phase::new("resize", Total::Unknown), &stop);
let observer = pulse.observer();
// my_library::resize(input, &pulse); // accepts &dyn how_far::Pulse
// observer.try_snapshot();          // sample on a UI thread when ready
```

The library can run serial leaf phases through `how_far::Steps`, or declare
parallel/nested branches with the underlying `Pulse` trait. `PulseTree`
translates those declarations into this crate's tree,
and forwards every cancellation check to the supplied stop policy. Existing
`Phase`/`Progress` handles remain useful when the consumer plans a tree itself.

## Count work in three steps

```rust
use how_far_along::{Phase, Report, Stop, Total, Unstoppable, ProgressWithStop};
use how_far_along::ext::ProgressExt;

let mut phase = Phase::new("resize", Total::Exact(17));
let observer = phase.observer();
let work = ProgressWithStop::new(Unstoppable, phase.progress());

work.check().unwrap();                    // Before starting any work.
for rows in [16, 1] {
    // Process these rows successfully, then:
    work.step(rows).unwrap();             // advance(actual count), then check.
}
phase.finish().unwrap();                  // After workers join and batches flush.
assert_eq!(observer.snapshot().fraction(), Some(1.0));
```

1. A `Phase` owns the plan and terminal outcome.
2. Workers receive clonable `phase.progress()` handles and a cancellation policy.
3. An `Observer` samples when the application is ready to display or export progress.

For cancellation, replace `Unstoppable` with your existing `Stopper`/`StopToken`
or a `poll::ControlHandle`. Calling `control.cancel()` from another thread is
independent of progress sampling. `step(n)` counts work **already completed**
before checking cancellation; keep the initial `check()` and report the actual
partial-batch length.

An algorithm may still accept `impl Stop + Report` or the two independently.
`Report` requires only `advance(u64)`; `Stop` remains unchanged. The
`ProgressExt::step` helper lives in the lightweight `how-far` crate, so library
code can use it without depending on this tracker. Plain `check()` stays the
cheap hot path; `step()` polls at reporting points. Pass
`ProgressWithStop::new(Unstoppable, IgnoreProgress)` to compile both policies away. `&dyn Stop`
seams continue to work through `ProgressWithStop`; new nested APIs can take `&dyn Pulse`
instead and let the library plan phases without depending on this crate.

## Add structure when it helps

```rust
use how_far_along::{Execution, Part, Phase, Total};

let mut job = Phase::new("encode", Total::Unknown);
let [before, middle, after] = job.split(Execution::Sequence, [
    Part::new("prepare", 35, Total::Exact(1)),
    Part::new("parallel work", 30, Total::Unknown),
    Part::new("write", 35, Total::Exact(1)),
]).unwrap();
```

Split `middle` into `ForkJoin` children with their own units and totals. During
that group the fraction is `0.35 + 0.30 * middle_fraction`, independent of the
number of workers. Groups nest, so another join or a reduction can occur halfway
through any branch. A parent finishes explicitly after all its children finish;
declaring an execution model does not enforce a schedule or run work for you.
The [phases example](examples/phases.rs) runs this structure with scoped threads.

For a Rayon work queue, clone one `Progress` to count completed logical items.
Use `Execution::WorkPool { max_parallelism }` for the configured ceiling.
`ext::ReportExt::batched(NonZeroU64)` gives each worker a private report buffer,
which flushes on drop. Check cancellation at the appropriate cadence; the
per-worker buffer deliberately reports only when it flushes, so its raw
`advance()` method does not poll.
Rayon `for_each_init` creates job-local state, not exactly one state per thread.

## Test a library's checkpoint cadence

Enable `diagnostics` in a dev-dependency to time report gaps and callbacks and
turn a completed `Trace` into source-located suggestions. Wrap the public
`&dyn Pulse` with `DiagnosticPulse` without changing the library's signature.
The same profiler can wrap an `enough::Stop` for cancellation-only libraries.
See the [testing and tuning guide](../../docs/how-far-testing-and-tuning.md)
for a fixture, threshold settings, and the limits of stage-weight estimates.

`Phase` is the unique lifecycle owner; cloning a `Progress` never grants finish
authority. Dropping an owner records `Abandoned`. Success and explicit `Skipped`
discharge the planned weight; cancellation and failure preserve partial counts.
Reaching a denominator does not mark success. Exact overruns and saturation remain
visible even on successful records. `Exact(0)` starts at zero until finished.

`Total::Unknown` produces no fraction until resolved or successfully finished.
`set_total` retains estimate revisions and permits honest regression. Reserve
unknown subtrees for discovery; a live partition cannot gain siblings and silently
renormalize its existing weights. `Snapshot::unresolved_fraction` exposes the
budget that still has no usable denominator. Counted fraction is not elapsed-time
fraction or ETA; an opaque straggler can dominate a nearly-complete fork/join.

## Choose how observation runs

`Observer::snapshot()` pulls an owned tree. On the browser UI thread, use
`Observer::try_snapshot()` and retry a busy metadata read on the next event-loop
turn. No callbacks run unless you poll.

* `poll::LocalPoller::poll(&mut self)` invokes `FnMut` on its caller's thread.
  Captures may be borrowed or thread-affine, including `Rc` and UI handles.
* `poll::SharedPoller::try_poll()` invokes `Send + Sync` callbacks on whichever
  polling worker wins the dispatch claim. Concurrent or recursive polls return
  busy immediately, while still observing shared cancellation.
* A callback can post a snapshot/notification to your executor or bounded queue.
  This is how a worker sends progress to a browser UI or a server telemetry task.

```rust
use how_far_along::{Phase, Total};
use how_far_along::poll::{Control, ControlHandle, LocalPoller};

let phase = Phase::new("job", Total::Unknown);
let control = ControlHandle::new();
let mut poller = LocalPoller::new(phase.observer(), control.clone());
poller.subscribe(|event| {
    // Arbitrary application work can run here without building a snapshot.
    // event.snapshot() builds one lazily, once per dispatch.
    // UI callbacks use event.try_snapshot() and retry a busy read next turn.
    // event.snapshot_owned() retains this observation; observer().clone()
    // allows a deferred consumer to obtain a later one instead.
    if event.control().is_cancelled() { Control::Cancel } else { Control::Continue }
});
poller.poll();
```

`ControlHandle` implements `Stop`; `cancel()` is latched independently of callback
cadence. `PollingStop::new(stop, shared_poller)` opts existing checks into shared
callback dispatch. It checks the wrapped stop once and observes callback-requested
cancellation before returning. Its `may_stop()` stays true through `StopToken`
conversion. Shared callbacks are configured before `build()`/sharing; local
subscriptions may be added or removed between polls. Panics propagate, and the
shared dispatch claim is released during unwinding.

Callbacks may request `Control::Yield`. A capable host driver consumes
`control.take_yield()` and actually returns, suspends, or awaits. A yield is never
encoded as a cancellation error. An ordinary synchronous Wasm callback that
schedules a timer cannot free the event loop. JSPI/Asyncify require the matching
Wasm build and host boundary; see [the host integration notes](../../docs/how-far-implementation.md).

There is no hidden debouncing policy. The consumer chooses clock reads, timer
frequency, coalescing, and callback cost. Cheap cancellation checks remain separate
from expensive notification work. Poll explicitly once after joining for terminal
delivery; frozen final snapshots remain available even if no callback requests one.

## Opt into profiling

Enable `profile` (requires `std`) and create `profile::Profiler` with a clock and
a finished-span retention limit. `StdClock` uses `Instant`; custom clocks support
browser hooks and deterministic tests. Create one span per task/chunk/attempt,
then call `span.instrument(work)` for original-site checks and reports. Nested
`Queued`, `Wait`, `Callback`, and `Yield` spans describe host execution.

The collector separates checks, report calls, and completed units. Per-task gap
measurements include entry and exit, so a busy worker cannot hide another worker's
unpolled tail. It also records time inside wrapped checks (including callbacks).
Record cancellation at the actual request and operation return after joins to
measure both detection latency and the longer cancellation-to-return latency.

`Trace::overlap(&span_ids)` reports wall time, summed task-time, mean/peak active
tasks, and the final single-task tail for an explicit independent group. It rejects
ancestor/descendant pairs to avoid double counting. Four spans ending at
12/20/22/90 ms have 90 ms wall time, 144 task-ms, mean concurrency 1.6, and a 68 ms
single-task tail. This is execution evidence, not measured CPU utilization or proof
of a scheduling defect. ETAs and model fitting belong to consumers.

`trace.with_progress(observer.snapshot())` attaches the phase plan, units, totals,
revisions and outcomes. `write_json` exports a versioned artifact with explicit
coverage; `Display` provides a readable task table. Include effective codec config,
run/attempt ID, build/hardware identity and predictor version via `metadata`.
There is no unbounded per-check event log: each live span retains at most 64 sites,
and dropped spans/unattributed calls are visible. See the [profile example](examples/profile.rs).

## Feature and synchronization boundaries

`how-far` has no features: `Report`, `IgnoreProgress`, and pointer forwarding are
always the same. Both crates require alloc; `how-far-along` constructs allocated state. Its tree/polling
API is identical with or without std; there is no `alloc` switch.

| Tracker build | Synchronization and capabilities |
| --- | --- |
| Default (`std`) | Short standard mutexes protect metadata handles; atomics count work and latch control |
| `default-features = false` | Same trees/pollers with alloc and the application's `critical-section` provider |
| `features = ["profile"]` | Adds bounded profiling with standard mutexes, clocks, and JSON/text export |
| `features = ["diagnostics"]` | Adds timed report gaps, callback timing helpers, and source-located tuning advice; implies `profile` |

Both crates use `#![forbid(unsafe_code)]`. Normal reporting, cancellation, and
callback dispatch use `core::sync::atomic`; they do not acquire metadata locks.
Only the interested consumer compiles the tracking machinery. Its runtime
dependencies are `how-far`, `enough`, and `critical-section`; no proc macros,
serialization framework, timer, executor, or spin-lock fallback is included.

Each node stores an immutable `Arc` of its current metadata. Readers clone that
handle inside a short lock, then release the lock before copying metadata,
walking the tree, allocating snapshots, or invoking callbacks. Writers allocate
before replacing the handle and drop old versions after releasing the lock.
Old versions are reclaimed once their readers release them; total-revision
history remains part of the current metadata for inspection.

`Observer::try_snapshot()` and `PollEvent::try_snapshot()` return `None` if any
std metadata mutex is busy. UI code retries on its next event-loop turn; there
is no retry loop or spin fallback. `snapshot()` is the blocking worker/native
convenience. Cancellation remains independent of either observation method.

No-std applications must provide the platform's
[`critical-section` implementation](https://docs.rs/critical-section/1.2.0/critical_section/),
usually through a HAL/RTOS feature. The provider must exclude every thread/core
that accesses the tree. That provider controls critical-section entry latency;
`try_snapshot` cannot make a blocking platform critical section nonblocking.
Only a handle clone/replacement occurs inside it. Host tests supply the standard
provider; threaded browser fixtures use the std backend and nonblocking UI reads.

Tracking needs pointer-sized atomic CAS. Counts saturate at `u64::MAX` where
64-bit atomics exist, otherwise at `usize::MAX`, with an overflow diagnostic.
Snapshots allocate and are not an interrupt-handler API. Live trees are weakly
consistent across nodes. Join workers and flush batches before finishing; an
owner cannot infer whether an external thread has joined.

The optional profiler belongs on native threads or browser workers. Supply a
browser clock hook instead of `StdClock`: `std::time::Instant` is not a portable
clock on `wasm32-unknown-unknown`. The browser fixture tests a JavaScript clock
on its owner Worker. Browser UI
readers use `Profiler::try_snapshot()`. Application callbacks and clock hooks run
outside profiler locks. No-std consumers can implement their own `Stop`/`Report`
instrumentation; this built-in collector requires std.

## Tested host scenarios

CI tests manual threads, asymmetric Rayon queues, nested and repeated joins,
codec-style preparation/search/filter/pack phases, terminal CLI output, and a
Tokio request disconnect cancelling and joining blocking work. Deterministic
profiling tests exercise stragglers, poll storms, callback cost and cancellation
latency. Miri checks metadata replacement and concurrent snapshots.

The [browser fixture](../../dev/how-far-browser/README.md) runs a real
`wasm-bindgen-rayon` pool in Chromium and WebKit. The UI samples shared Rust
progress and cancels the worker while remaining responsive. Main-thread tests
exercise native JSPI or resumable chunks on engines without it. These are host
integration tests, not a production codec port or certification of packaged Safari.

`cargo test -p how-far-along --all-features` runs the native scenarios; browser builds and
tests have separate CI jobs. [The full test matrix](../../docs/how-far-implementation.md#test-matrix)
documents the platform boundaries and reproducible build/performance measurements.

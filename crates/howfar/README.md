# howfar

Count completed work. Check cancellation. Observe when you choose.

`howfar` depends only on the existing zero-dependency `enough` crate. It has
no runtime, timers, proc macros, serialization dependencies, or executor glue.
**MSRV: Rust 1.88.** `enough` and `almost-enough` retain Rust 1.85. The browser
test application's threaded Wasm build uses nightly; the library does not.

```toml
[dependencies]
howfar = "0.1"
# Embedded/no_std with shared progress trees:
# howfar = { version = "0.1", default-features = false, features = ["alloc"] }
```

## Count work in three steps

```rust
use howfar::{Phase, Report, Stop, Total, Unstoppable, Work};
use howfar::ext::WorkExt;

let mut phase = Phase::new("resize", Total::Exact(17));
let observer = phase.observer();
let work = Work::new(Unstoppable, phase.progress());

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

An algorithm accepts `impl Stop + Report`, or accepts the two independently.
`Report` requires only `advance(u64)`; `Stop` remains unchanged. Pass
`Work::new(Unstoppable, NoProgress)` to compile both policies away. `&dyn Stop`
seams continue to work through `Work`, but cannot discover a reporting interface
after type erasure: add `with_progress` or a `Report` parameter where work is counted.

## Add structure when it helps

```rust
use howfar::{Execution, Part, Phase, Total};

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
which flushes on drop. Check cancellation separately at the appropriate cadence.
Rayon `for_each_init` creates job-local state, not exactly one state per thread.

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

`Observer::snapshot()` pulls an owned tree. No callbacks run unless you poll.

* `poll::LocalPoller::poll(&mut self)` invokes `FnMut` on its caller's thread.
  Captures may be borrowed or thread-affine, including `Rc` and UI handles.
* `poll::SharedPoller::try_poll()` invokes `Send + Sync` callbacks on whichever
  polling worker wins the dispatch claim. Concurrent or recursive polls return
  busy immediately, while still observing shared cancellation.
* A callback can post a snapshot/notification to your executor or bounded queue.
  This is how a worker sends progress to a browser UI or a server telemetry task.

```rust
use howfar::{Phase, Total};
use howfar::poll::{Control, ControlHandle, LocalPoller};

let phase = Phase::new("job", Total::Unknown);
let control = ControlHandle::new();
let mut poller = LocalPoller::new(phase.observer(), control.clone());
poller.subscribe(|event| {
    // Arbitrary application work can run here without building a snapshot.
    // event.snapshot() builds one lazily, once per dispatch.
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
Wasm build and host boundary; see [the host integration notes](../../docs/howfar-implementation.md).

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

| Build | Available |
| --- | --- |
| `default-features = false` | Core `Report`, `Work`, no-op sink, batching extensions; no allocation |
| `default-features = false, features = ["alloc"]` | Phase trees, atomic counters, snapshots/JSON, control handles and pollers |
| Default (`std`) | Same public progress API with the standard library enabled |
| `features = ["profile"]` | Bounded profiler with OS mutexes, clocks, and JSON/text export |

Counters use `core::sync::atomic`. Reporting, phase metadata reads and polling
never acquire a blocking mutex or spin lock. Metadata is immutable after
publication; previous versions are retained until the job is dropped. This uses
a small internal unsafe publication primitive with safety comments and Miri tests.
Create new trees for retries/requests and drop old observers when retention ends.
Metadata revisions are cold operations, not a substitute for `advance`: repeated
revisions retain previous metadata and revision histories for the job's lifetime.

`alloc` needs pointer-sized atomic CAS. Counts saturate at `u64::MAX` on targets
with 64-bit atomics (including wasm32), otherwise at `usize::MAX`, with an explicit
overflow diagnostic. This preserves lock-free counters on Cortex-M instead of
hiding a lock in `u64` arithmetic. Snapshots allocate; they are not an interrupt
handler API. Live trees are weakly consistent between nodes. Join workers and
flush batches before finish; the owner cannot infer that an external thread joined.

The profiler uses `std::sync::Mutex`, never a spin fallback. In threaded Wasm,
run profiling work in workers; a UI reader uses `Profiler::try_snapshot()` and
retries on its next event-loop turn. No application callback or clock hook runs
while profiler locks are held. No-std consumers can implement `Stop`/`Report`
adapters for their platform's instrumentation; the built-in collector needs `std`.

## Tested host scenarios

CI tests manual threads, asymmetric Rayon queues, nested and repeated joins,
codec-style preparation/search/filter/pack phases, terminal CLI output, and a
Tokio request disconnect cancelling and joining blocking work. Deterministic
profiling tests exercise stragglers, poll storms, callback cost and cancellation
latency. Miri checks immutable publication and concurrent snapshots.

The [browser fixture](../../dev/howfar-browser/README.md) runs a real
`wasm-bindgen-rayon` pool in Chromium and WebKit. The UI samples shared Rust
progress and cancels the worker while remaining responsive. Main-thread tests
exercise native JSPI or resumable chunks on engines without it. These are host
integration tests, not a production codec port or certification of packaged Safari.

`cargo test -p howfar --all-features` runs the native scenarios; browser builds and
tests have separate CI jobs. [The full test matrix](../../docs/howfar-implementation.md#test-matrix)
documents the platform boundaries and reproducible build/performance measurements.

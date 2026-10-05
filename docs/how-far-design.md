# how-far design

Libraries describe and report work. Owners finish it. Checkpoints run cancellation
and application callbacks. Observers read the resulting state. The current
implementation is `codex/howfar-v2` at `a1d0286`, extending
callback baseline `0adafe1`. The intended deliverable is its full delta from
main, not just that baseline. See [the handoff](how-far-handoff-opus.md) for
branch state and unfinished integration work.

## Interface and ownership

Libraries depend on `how-far` and accept `&dyn Pulse`. `check` may invoke a
checkpoint callback; `advance` only records completed units; `step` does both in
that order. Expensive codec loops stay non-generic over their pulse.

`split` hands out child owners. Completion consumes an owner; libraries never
finish their borrowed input. `Stages::run` returns the library's normal Result,
as does `Phases::run` for independently selected attempts. `complete(result)` or
`result.finish_phase(owner)` explicitly communicates the enclosing result.
Untouched phases become Skipped on success and NotRun on error. Started phases
without a handoff become Abandoned. A recovered parent can succeed while
retaining failed child attempts. See [API boundaries and states](how-far-api.md).

Borrowed error classification recognizes StopReason without converting the
library error. Explicit classifiers accommodate foreign wrappers. Rejected
observations diagnose protocol misuse and use cancellation-preserving untracked
children. `TryStages` is the opt-in `checked` layer for callers who want plan
errors in their return type. The normal API never introduces them.

Owners and guards are `must_use`: discarding construction warns, but holding a
guard cannot prove it is eventually completed. Consumption prevents duplicate
completion; a private runner enum prevents contradictory lifecycle flags.

`share()` returns a cloneable owned pulse with the same planning, cancellation,
start and total-revision capabilities. It never transfers completion rights.
Unsupported ownership returns `NotShareable`. References, boxes, Arcs, children,
callback pulses and diagnostic wrappers forward these operations. `share()` is
the only owned view: a `SharedPulse` works wherever a `Stop` or `Report` is
expected, such as a codec context that stores `impl Stop + 'static`.

The root/child owner holds one completion token separate from shared state. A
retained shared handle cannot extend the lifecycle. Reports after completion are
ignored, and terminal observations are frozen. Administration (start, total
revision, split, completion) runs under one short per-phase lock that never
spans user code, so the owner and shared views queue instead of refusing each
other. Dropping the owner waits at most for an administrative call already
under way, then records abandonment outside the lock.

A phase can start before discovering its plan. Only a nonzero report makes it a
counting leaf and prevents a split; zero reports are inert. Totals are revisable
through `Pulse`, with revision history retained. A branch cannot revise its total.
NoPulse validates plan shapes but retains no lifecycle state, so it cannot detect
stateful misuse. Unsupported total revision is an explicit error.

## Accounting and callbacks

`how-far-along` owns one accounting implementation shared by `PulseTree` and
`FnPulse`. Reports update a leaf counter; they do not update a job-wide fraction.
This avoids a shared global counter coupling otherwise independent workers.

`Observer::summary` walks the subtree without copying names or allocating a
snapshot. `try_summary` skips a busy metadata read. Full snapshots explicitly
allocate their retained records. Fractions can regress after a total revision,
and remain unknown when a required denominator is unknown or overflowed.
Outcomes remain independent of the numeric fraction.

The optional `callback` feature adds FnPulse. Each check offers a lazy Checkpoint
view with a phase ID, name, summary and snapshot accessors. It does no tree walk
unless requested. Callbacks can overlap; first stop reason wins and in-flight
callbacks may complete. No internal lock is held during user code. Callbacks
must not recursively check the same pulse. LocalPoller remains available for
thread-affine FnMut/UI callbacks driven by the host.

Reports, phase finish and Drop never invoke checkpoint callbacks. An application
observes once after finishing the root for its final display update. Paced::finish
flushes the short final batch and checks cancellation; its Drop only counts the
pending completed units. Phase planning and destruction can allocate; reporting
and ordinary non-reaching paced steps do not.

## Synchronization and portability

Counters use saturating relaxed atomic updates. These counts carry no application
data and establish no happens-before relation for encoder buffers. Thread/Rayon
joins establish completion of worker writes. Phase-state publication uses
release/acquire so terminal readers see the stored final count and outcome.
Changing these orderings is a correctness decision, never a performance feature.

Metadata uses immutable Arc versions replaced under short platform locks.
Snapshots clone the version under the lock, then walk/allocate outside it. No
lock or critical section is held across callbacks or host suspension. The no_std
backend requires the application's critical-section provider to exclude every
participating core/thread; disabling interrupts on just one core is insufficient.
On that backend, even `try_summary` enters the provider's critical section:
it avoids a conflicting borrow but is not a portable wait-free guarantee.

Core and tracker require pointer atomics for Arc. Native 64-bit counters are used
where available. Other supported targets use native-width saturation and expose
the limit through Snapshot::counter_max; callback support no longer requires
64-bit atomics. Pointer-atomic-free MCUs are not supported by this implementation.
A future portable-atomic backend would also need portable shared ownership, not
just replacement counters. It must not silently change the current hot path.

On targets without 64-bit atomics, a critical-section-backed u64 counter is a
possible opt-in backend, with a measurable per-report exclusion cost. It is not
implemented here: the current native-width backend retains its explicit overflow
behavior. Use larger units and per-worker pacing where appropriate.

Pacing reduces cache-line transfers and CAS retries for a shared counter. Giving
workers distinct children removes contention only when separate totals/outcomes
are semantically useful; worker count must not change the containing phase's
weight. A relaxed RMW still has cache coherence cost. A shared counter is not a
reason to use SeqCst, and Relaxed is not a substitute for batching.

## Compile-time seams

The core depends only on enough. Its optional `adapters` feature adds WithStop,
including cancellation-only construction, borrowed extra policies and explicit
replacement (which bypasses inner callbacks too). `checked` adds the strict
legacy runner; neither feature changes protocol behavior. An erased implementation avoids
monomorphizing adapter machinery per wrapped type. Traits, method semantics and
layout do not vary by feature.

Tracker defaults are `std` and `json`. `callback` adds synchronous checkpoint
callbacks; `adapters` forwards the core convenience feature. Profiling and diagnostics are a separate `how-far-really` crate requiring
std and tracker JSON. With default features disabled the
tracker uses alloc, native atomics and the host critical-section implementation,
without JSON formatting, callbacks, profiling, clocks or a runtime.

Features add items and do not silently suppress observations through existing
items. Applications may feature-gate adoption in an encoder to remove both its
how-far dependency and every reporting site. The zenpng fixture demonstrates
this on the actual encoder, while retaining its original Stop API.

## Wasm

The same Pulse and shared handles work in worker-owned Rayon pools. Wasm binding
and thread-pool dependencies live solely in the browser fixture. The fixture
uses shared memory, the proper atomic std build, and cross-origin isolation.
UI reads use nonblocking observations; cancellation is a shared atomic request,
not a message that requires the busy worker to process an event-loop turn.

Ordinary callbacks do not yield. The host chooses workers, resumable chunks, or
an explicit JSPI suspension boundary. Never hold locks across that boundary.
The raw probe tests timer turns, suspension/resumption/cancellation and worker
progress. The browser fixture tests Chromium and Playwright WebKit, not packaged
Safari. Abandonment through Drop requires normal destruction/unwinding; aborts
and worker termination do not run Rust destructors.

## Validation

See [the current validation report](../benchmarks/howfar-results-validation.md)
for the Result-based API, separate diagnostics crate, measured results and
reproduction commands. [The earlier report](../benchmarks/howfar-v2-validation.md)
retains measurements for its earlier implementation. The test suite covers
ownership, nested spawned libraries, revisions,
zero counts, saturation, concurrent callbacks, final paced cancellation, stale
handles, diagnostic forwarding and dropping an owner during administration.

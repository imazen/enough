# how-far design and API boundaries

Libraries describe and report work. Owners finish it. Checkpoints run
cancellation and application callbacks. Observers read the resulting state.
Encoder adoption that is still in progress is recorded in
[the zenav1 measurement](how-far-zenav1-measurement.md); what is tested is in
[the validation notes](how-far-validation.md).

## Crates

The crates follow one dependency direction:

```text
library → how-far → enough
application → how-far-along → how-far
application/tests → how-far-really → how-far-along + how-far
```

`how-far` owns the portable protocol: `Pulse`, `Stop`, `Report`, planning
specs, `Child`, `SharedPulse`, `Stages`, `Phases`, pacing and result handoffs,
plus the two pulses that observe nothing: `NoPulse` and the cancellation-only
`StopOnly`. `IsStop` lives in `enough`, which libraries already depend on, and
is re-exported. Libraries expose `&dyn Pulse` and their own error types, never
tracker owners or diagnostic records unless observation is their purpose.

`how-far-along` owns tracking: `Phase`, `PulseTree`, `Reporter`, `Observer`,
`NodeId`, `Status`, `Summary`, `Snapshot`, polling and optional callbacks. Its
root re-exports the author-facing core types, not `ChildPulse`, `RunError` or
`TryStages`, which implementors import from core. It never re-exports
diagnostics. `how-far-really` owns opt-in profiling and analysis in two public
modules, `profile` and `diagnostics`, with no root re-exports. The API
inventory under `docs/public-api` and compile-fail doctests guard these
boundaries.

## Interface and results

A library accepts `&dyn Pulse`. `check` asks whether to stop and may run a
checkpoint callback; `advance` only counts completed units; `step` does both, in
that order. Hot codec loops stay non-generic over their pulse.

`Stages` runs a sequence and stops assigning stages after a work error; `Phases`
runs independently chosen attempts, so the library decides what is recoverable.
Both return the library's own `Result` unchanged. `Complete` consumes an owner
and returns the exact original `Result`: `complete(result)`,
`result.finish_phase(owner)`, and `complete_with(|owner| ...)`, which runs a
multi-step body so an early `?` cannot skip the handoff. `From<StopReason>`
lets library code use `?` at a checkpoint; `IsStop` tells an owner whether an
error is a cancellation. It is a trait rather than a
`TryFrom<&LibraryError> for StopReason` convention because a bound on that
conversion lets rustc infer `StopReason` for an unannotated error and report a
misleading mismatch when the conversion is missing; a missing `IsStop` is
reported as such. Foreign error wrappers use the `_classified` variants. No
planning error ever enters a library's `Result`: best-effort planning records a
rejected plan and supplies untracked children that still check the stop. The
optional `checked` feature keeps `TryStages`, for callers who want planning
failures in their return type.

`split` hands out child owners; libraries never finish their borrowed input.
Completion rights are owned and consumed, so completing twice through one owner
is unrepresentable, and a private runner enum prevents contradictory lifecycle
flags. Owners and guards are `must_use`, which catches discarding one but cannot
prove it is eventually completed. A repeated or unknown stage index is recorded
and runs through an untracked view, which keeps the work's result but does not
make running unintended work correct.

`share()` returns a cloneable, `'static` `SharedPulse` with the same planning,
cancellation, start and total-revision capabilities, but never completion
rights. It works wherever a `Stop` or `Report` is expected, such as a codec
context that stores `impl Stop + 'static`; join such work before finishing the
original owner. A pulse that cannot be owned, such as a borrowed stop policy,
returns `NotShareable`: no path panics or silently drops cancellation.
References, boxes, `Arc`s, children, callback pulses and diagnostic wrappers
forward every operation.

## States

| State/outcome | Meaning |
| --- | --- |
| NotStarted (default) | Declared, no observed entry or nonzero report |
| Running | Entered or reported work, no completion |
| Succeeded | Owner reports success |
| Failed | Owner reports an ordinary failure |
| Cancelled | Owner reports a recognized stop reason |
| Skipped | Work is unnecessary; its obligation is discharged |
| NotRun | Earlier failure/stop prevented work; earns no completed weight |
| Abandoned | Owner disappeared or parent closed after entry without a result |

`Status::Finished(Outcome)` keeps lifecycle separate from outcome, and the count
fraction is independent evidence: a cancelled phase may have counted all its
units, and a successful one may have missing reports. On completion, untouched
children become Skipped on success and NotRun on failure, started unfinished
ones become Abandoned, and finished children keep their outcomes, including
failed attempts under a recovered parent. `completion_inferred` marks a phase
resolved by its parent rather than by its own handoff: provenance, not proof
that the program was correct. Dropping a plan or root without its `Result`
records abandonment; Drop cannot see a `Result`.

## Lifecycle and administration

The owner holds one completion token separate from shared state, so a retained
shared view cannot extend the lifecycle. Reports after completion are ignored
and terminal observations are frozen. Administration (start, total revision,
split, completion) runs under one short per-phase lock that never spans user
code, so the owner and shared views queue instead of refusing each other.
Dropping the owner waits at most for an administrative call already under way,
then records abandonment outside the lock.

A phase can start before it knows its plan. A nonzero report makes it a
counting leaf and prevents a split; zero reports are inert. A leaf's total is
revisable, and its 16 most recent revisions are kept; a split clears them,
since a branch has no total. Phases nobody observes (`NoPulse`, `StopOnly`,
untracked children) accept and discard revisions. `NoPulse` validates plan
shapes but keeps no lifecycle state, so it cannot detect stateful misuse.

## Accounting and callbacks

`PulseTree` and `FnPulse` share one accounting implementation. Reports update
a leaf counter, never a job-wide fraction, so independent workers do not
contend on one counter. `Observer::summary` walks the subtree without copying
names or allocating; `try_summary` skips a busy metadata read; snapshots
allocate their records. Fractions can regress after a total revision and stay
unknown while a required denominator is unknown or overflowed.

The optional `callback` feature adds `FnPulse`. Each check offers a lazy view
(phase, name, summary and snapshot accessors) that walks nothing unless asked.
Callbacks can overlap; the first stop reason wins. No internal lock is held
during user code, and callbacks must not recursively check the same pulse.
Reports, phase completion and Drop never invoke callbacks; an application
observes once after finishing the root. `LocalPoller` drives thread-affine UI
callbacks from the host. `Paced::finish` flushes the final batch and checks;
its Drop only counts.

## Synchronization and portability

Counters use saturating relaxed atomics. They carry no application data and
establish no happens-before relation for encoder buffers; thread and Rayon
joins do. Phase state is published with release/acquire so terminal readers see
the final count and outcome. Changing these orderings is a correctness
decision, not a performance knob.

Metadata is an immutable `Arc` version replaced under a short platform lock;
snapshots clone the version under the lock and walk it outside. No lock or
critical section spans a callback or host suspension. The `no_std` backend
needs a critical-section provider that excludes every participating core;
disabling interrupts on one core is not enough, and there even `try_summary`
enters the critical section.

Core and tracker need pointer atomics for `Arc`. Counters are native 64-bit
where available, saturating at 2^63 - 1 so that a report is one `fetch_add`
that never retries when workers share a phase, and native-width with
compare-and-swap saturation elsewhere (`Snapshot::counter_max`). Pacing
reduces cache-line transfers on a shared counter; giving workers separate children helps only when separate totals or
outcomes mean something, and worker count must not change the containing
phase's weight.

## Compile-time seams

The core depends only on `enough`. Its `adapters` feature adds `WithStop`
(combined or replacing, owned or borrowed stop policies); `checked` adds the
strict runner. Neither changes protocol behaviour or layout, and erased
implementations avoid compiling adapter machinery per wrapped type. Tracker
defaults are `std` and `json`; `callback` and `adapters` are additive. Without
defaults the tracker needs only alloc, native atomics and the critical-section
provider. Diagnostics is a separate std-only crate. An encoder can feature-gate
both its how-far dependency and every reporting site, as the zenpng fixture
does.

Extensible enums and observation records are non-exhaustive. Diagnostic
messages are not machine identifiers, and timing thresholds are not correctness
rules. IDs identify records within a process. Tracking JSON is schema 2; trace
JSON embeds a complete versioned tracking document, and schema changes need an
explicit version change.

## Wasm

The browser's main thread cannot block, and a worker running a synchronous
Rust loop cannot receive a message until the loop returns. There are three
ways to stay responsive:

1. **Keep CPU work in workers** and read progress from the UI thread with
   `Observer::try_snapshot`, retrying a busy read on the next frame. A
   cancel request reaches the worker's loop through shared memory: a stop
   policy such as `almost_enough::Stopper`, flipped from the UI thread.
   Terminating a worker is a hard abort that runs no Rust cleanup, so no
   abandonment is recorded.
2. **Return to the host at safe boundaries.** A resumable algorithm runs a
   bounded chunk, returns, and is called again on the next turn. A time budget
   is checked only at those boundaries, so it cannot shorten one indivisible
   chunk.
3. **Suspend the stack** at a Wasm import with JSPI, or with an Asyncify
   build, and keep synchronous Rust source.

JSPI's host side looks like this:

```js
const imports = {
  host: {
    checkpoint: new WebAssembly.Suspending(async (completed) => {
      reportProgress(completed);
      if (performance.now() >= deadline) {
        await new Promise(resolve => setTimeout(resolve, 0));
        deadline = performance.now() + 10;
      }
      return cancelled ? 1 : 0;
    }),
  },
};
const { instance } = await WebAssembly.instantiate(bytes, imports);
const completed = await WebAssembly.promising(instance.exports.run)(units);
```

The callback must reach a suspending import, and the outer call must go
through the Wasm export itself; ordinary JavaScript frames in between, such as
an arbitrary wasm-bindgen closure trampoline, can prevent suspension. Do not
re-enter a mutably borrowed encoder while it is suspended, and do not hold
application locks across a suspension. See the
[JSPI proposal](https://github.com/WebAssembly/js-promise-integration/blob/main/proposals/js-promise-integration/Overview.md).
Safari 27 added JSPI according to the
[WebKit release notes](https://webkit.org/blog/18325/webkit-features-for-safari-27-0/#webassembly);
feature-detect `WebAssembly.Suspending` and `WebAssembly.promising`, and fall
back to workers, chunks, or
[Asyncify](https://emscripten.org/docs/porting/asyncify.html). A callback that
schedules a timer and returns to the same Rust loop does not yield, and
neither does an already-resolved promise.

The [Wasm probe](../dev/how-far-wasm/README.md) runs real callbacks in Wasm
under Node with JSPI, and checks the timer boundary, suspension and
cancellation, and progress posted from a worker. The
[browser fixture](../dev/how-far-browser/README.md) runs a
`wasm-bindgen-rayon` pool in a worker, in Chromium and Playwright's WebKit; its
Wasm and thread-pool dependencies live only there. A second binding context on
the UI thread reads the same Rust tree and cancels through shared memory.
Threaded Wasm needs shared memory, cross-origin isolation, a worker-pool
initializer, and a standard library rebuilt with atomics; see
[wasm-bindgen-rayon](https://github.com/RReverser/wasm-bindgen-rayon). The
crates do not configure that build for you. Playwright's WebKit is not Apple's
Safari, and Asyncify builds are not covered.

The profiler needs `std` but no OS threads. On the web, supply a
`performance.now()` clock instead of `StdClock`, record on a worker, and read
traces from the UI with `Profiler::try_snapshot`.

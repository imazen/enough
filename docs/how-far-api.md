# API boundaries and evolution

The crates follow one dependency direction:

```text
library → how-far → enough
application → how-far-along → how-far
application/tests → how-far-really → how-far-along + how-far
```

`how-far` owns the portable protocol: `Pulse`, `Stop`, `Report`, planning specs,
`Child`, `SharedPulse`, `Stages`, `Phases`, pacing and result handoffs. Libraries
should expose `&dyn Pulse` and their own error types. Re-export selected core
types if that simplifies adoption. Do not expose tracker owners or diagnostic
records through a codec's API unless observation itself is its purpose.

`how-far-along` owns concrete tracking: `Phase`, `PulseTree`, `Reporter`,
`Observer`, `NodeId`, `Status`, `Summary`, `Snapshot`, polling and optional callback
adapters. Its root re-exports the author-facing core types, not `ChildPulse`,
legacy count/check handle implementation types, `RunError`, or `TryStages`.
Implementors import those directly from core. It never re-exports diagnostics.

`how-far-really` owns opt-in profiling and analysis. Its two public modules are
`profile` and `diagnostics`. It has no convenience root re-exports, and exports
no imported core or tracker types. Signatures necessarily refer to those types;
that is a dependency, not an additional public import path. Applications import
types from their owning crate. The API inventory under `docs/public-api` and
compile-fail doctests guard these boundaries.

## Results and ownership

`Complete` consumes an owner. Its methods return the exact original Result;
classification borrows the error and requires neither cloning nor converting it.
`complete_with` runs a multi-step body and hands over its result, so an early
`?` inside the body cannot skip the handoff.
`From<StopReason>` is only for library code using `?` at a checkpoint.
`TryFrom<&LibraryError> for StopReason` identifies cancellation for observation.
Foreign error wrappers can use `run_classified` and `complete_classified` to
avoid Rust's orphan-rule restriction. No PlanError conversion is required.

`Stages` stops assigning declared phases after a work error. `Phases` permits
independently chosen attempts, so the library decides what is recoverable.
`TryStages` is the optional `checked` runner for callers who explicitly want
planning failures in their return type; its `RunError<E>` and `run_stoppable`
are not the normal adoption path. All low-level administration methods retain
explicit errors. Busy means overlapping administration of one owner, not a
failed encode or a locked progress counter. Best-effort planning records that
rejection and supplies cancellation-preserving untracked children.

Completion rights are owned and consumed; shared views never acquire them.
That makes duplicate completion through the same handle unrepresentable.
The runner's private enum prevents contradictory running/stopped flags. Runtime
indices and caller omissions remain representable; reporting bugs must not turn
into replacement codec errors. The normal helper returns the work result even
on invalid/repeated stage selection, recording a diagnostic and using an untracked
view. This does not make executing unintended work correct.

## State vocabulary

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

`Status::Finished(Outcome)` keeps lifecycle separate from outcome. Count fraction
is independent evidence: a cancelled phase may have counted all its units, and
a successful phase may have missing reports. `completion_inferred` distinguishes
parent/helper resolution from an explicit child handoff. It is deliberately a
provenance flag, not a claim that the program was correct.

Success resolves untouched phases as Skipped; failure resolves them as NotRun;
started unfinished phases become Abandoned. Existing finished child outcomes
are retained, including failed attempts under successful recovered parents.
Dropping a plan/root without its Result records abandonment. Dropping a Result
cannot communicate its variant to an unrelated owner.

## Feature and compatibility policy

Core is `no_std + alloc`, with only `enough` as a dependency. `adapters` adds
owned/borrowed stop composition; `checked` adds the strict legacy runner. No
feature changes the meaning of a Pulse method. Tracker `std`, `json`, `callback`
and `adapters` are independent additive seams. Diagnostics requires a separate
std-only crate. Encoder adoption can gate both dependency and instrumentation.

`WithStop::borrowed` combines checkpoints; `replacing_borrowed` bypasses the
original checkpoint policy, including callbacks. Both preserve reporting and
completion. Owned `new`/`replacing` policies support `share`; borrowed policies
support scoped workers and return NotShareable for ownership conversion. The
legacy infallible `handle()` cannot represent that error and panics for borrowed
policies; use `share()` or adapter `try_handle()` where ownership is optional.

Extensible enums and observation records are non-exhaustive. Do not use diagnostic
messages as machine identifiers or treat timing thresholds as correctness rules.
IDs identify records, not persistent objects across process runs. JSON schema 2
names NotStarted and exports completion provenance; trace JSON embeds a complete
versioned tracking document. Schema changes require explicit version changes.
Future diagnostic backend changes must stay out of the core protocol and the
tracker's default build. No promised stable struct layout beyond the documented
small handle footprint; no features silently remove a capability from an
already-available type.

# how-far-along

Tracking and observation for `how-far`. This crate records declared work;
applications choose when to observe it. It starts no threads and reads no clocks.

```rust
use how_far_along::{prelude::*, Phase, PhaseSpec, PulseTree, Stages, StopReason, Total, Unstoppable};
let tree = PulseTree::new(Phase::new("job", Total::Unknown), Unstoppable);
let observer = tree.observer();
let result = {
    let mut stages = Stages::new(&tree, &[
        PhaseSpec::new("work", 1, Total::Exact(2)),
        PhaseSpec::new("optional", 1, Total::Exact(1)),
    ]);
    let result = stages.run(|phase| phase.step(2));
    result.finish_phase(stages)
};
result.finish_phase(tree)?;
assert_eq!(observer.snapshot().fraction(), Some(1.0));
# Ok::<(), StopReason>(())
```

Every phase starts `Status::NotStarted`, becomes `Running` when entered or
reported to, and eventually becomes `Finished(Outcome)`. Outcomes distinguish
`Succeeded`, `Failed`, `Cancelled`, `Skipped`, `NotRun`, and `Abandoned`.
A successful parent may retain a failed attempt: the library's result decides
whether it recovered. `completion_inferred` identifies outcomes resolved by the
parent or a plan helper, rather than explicitly reported by that child.

A result handoff freezes the parent's observation. Started but uncompleted
children become abandoned; untouched children are skipped on success and not
run on failure. Retained child owners can close later but cannot rewrite that
frozen parent observation. Join workers first. Drop supplies abandonment, never
an inferred successful result.

`Observer::summary()` reads fraction, unresolved share and status without
allocating. `snapshot()` copies the tree. `try_summary` / `try_snapshot` avoid
waiting on standard metadata locks; on no_std they enter the host's critical
section and do not promise wait-free access. Counting never locks metadata.
Unknown totals and overflow stay visible; fractions can regress after revisions.
Success discharges a parent's obligation without rewriting failed attempts.

The default features are `std,json`. With defaults disabled, tracking uses
`alloc`, pointer atomics, and the application's critical-section provider. That
provider must exclude every participating core. Native-width counters saturate
and expose overflow on targets without 64-bit atomics. The `callback` feature
adds `FnPulse` and lazy `Checkpoint`; it does not require std or 64-bit atomics.
`adapters` forwards the core cancellation-adapter feature.

Callbacks run at checkpoints, never from counting, completion or Drop. Use
`poll::LocalPoller` for UI-thread or borrowed callbacks. Shared workers retain
full capabilities through `share()`; completion rights remain with the owner.

Only selected `how-far` author-facing types and its trait prelude are re-exported.
Implementor and checked-runner types are imported directly from `how_far`.
Profiling and tuning belong in `how_far_really::{profile, diagnostics}`; neither
that crate nor its types are re-exported here.

```compile_fail,E0432
use how_far_along::diagnostics::DiagnosticPulse;
```

Tracking JSON uses schema version 2 (`NotStarted` and inferred-completion evidence).
See [the example application](../../examples/how-far-app/README.md) and
[design and API boundaries](../../docs/how-far-design.md).

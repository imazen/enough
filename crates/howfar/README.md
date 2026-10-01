# howfar

A small, object-safe progress interface for libraries. A library accepts one
`&dyn Pulse` for cancellation, completed work, and nested phase planning. It
depends only on `howfar`; the caller decides whether to ignore reports or opt
into [`howfar-along`](../howfar-along/README.md) for a shared tree, snapshots,
callbacks, and profiling.

**Rust 1.88+, `no_std + alloc`, `#![forbid(unsafe_code)]`.** There are no feature
flags or macros. The only dependency is the small `enough` cancellation trait
crate; the reporting hot path does not allocate or read a clock.

```toml
[dependencies]
howfar = "0.1"
```

## One callback through a library

```rust
use howfar::{PhaseSpec, Pulse, RunError, Steps, StopReason, Total};

fn process(rows: &[u8], pulse: &dyn Pulse) -> Result<(), RunError<StopReason>> {
    let mut steps = Steps::new(pulse, &[
        PhaseSpec::new("rows", 1, Total::Exact(rows.len() as u64)).units("rows"),
    ])?;
    steps.run_stoppable(|stage| {
        for row in rows {
            stage.check()?;
            // Process the row successfully, then count it.
            let _ = row;
            stage.advance(1);
        }
        Ok(())
    })?;
    steps.finish()?;
    Ok(())
}

process(&[1, 2, 3], &howfar::NoPulse).unwrap();
```

`Steps` runs declared leaf phases in order. It marks a successful phase finished;
on a stop request, it marks that phase cancelled and later phases skipped.
Use `run` instead of `run_stoppable` when an error means failure rather than
cancellation. When both are possible, `run_classified` marks only errors the
library identifies as stops as cancelled; other errors mark the stage failed.
`RunError<E>` separates an operation error from a plan error,
so a library can use a type alias for its public error. A panic leaves work
abandoned. `NoPulse` is the zero-sized, non-cancelling choice.

For parallel work with one logical count, use ordinary `run_stoppable` and pass
its `&dyn Pulse` to every worker. Join the workers before returning from the
closure; `Steps` then finishes the phase. Give workers separate child phases
only when they need their own totals, statuses, or weights. In that case, use
`run_nested_stoppable` and `Pulse::split`; finish and join the children before
returning. `Steps` finishes the containing stage and keeps errors flat.

Child phases can split into `Sequence`, `ForkJoin`, or `WorkPool` groups. The
relative weights of siblings are fixed before work starts, so a parallel
middle phase can keep 30% of its parent's budget regardless of worker count.
`PhaseSpec` is built with `new` and its fields stay readable; it is
non-exhaustive so future planning options will not break library authors.

`Pulse` extends `Stop` and `Report`, so the same `&dyn Pulse` also works
at existing `&dyn Stop` and `&dyn Report` seams on this crate's Rust 1.88 MSRV.
`Stop` and `StopReason` are re-exported here, so a new library needs only
`howfar` in its manifest.
`check()` does not report completed work, and `advance()` does not poll or call
subscribers. The caller chooses both cadences.

## Thread tests

`cargo test -p howfar --test pulse_threads` starts four OS threads sharing one
`&dyn Pulse`. It verifies that their asymmetric reports add up and that a
cancellation request becomes visible to every worker. The tracker integration
tests (`cargo test -p howfar-along --test pulse`) cover the same shared phase
inside a serial → parallel → serial plan, including a live snapshot, and a
Rayon pool whose chunks all report to one logical phase.

## Count only

Algorithms that need no phase structure can continue to accept `impl Report`.
`IgnoreProgress` discards counts while preserving a separate cancellation
policy. References, `Box`, `Arc`, and `Option` forward `Report` without feature
switches. The interface is identical in every build.

## Opt into tracking in applications and tests

```toml
# In a library's Cargo.toml:
[dependencies]
howfar = "0.1"

[dev-dependencies]
howfar-along = { version = "0.1", features = ["profile"] }
```

Applications can use `howfar-along::PulseTree` to pair a phase tree with a stop
policy, pass it as `&dyn Pulse`, and sample through an observer. Its optional
plumbing stays out of the library dependency graph. Neither crate permits unsafe
code. See the [tracker example](../howfar-along/README.md) and
[validation notes](../../docs/howfar-implementation.md).

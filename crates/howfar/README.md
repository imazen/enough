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
cancellation. `RunError<E>` separates an operation error from a plan error,
so a library can use a type alias for its public error. A panic leaves work
abandoned. `NoPulse` is the zero-sized, non-cancelling choice.

For nested or parallel work, use the underlying `Pulse::split` and `finish`
methods. Child phases can split into `Sequence`, `ForkJoin`, or `WorkPool`
groups. The relative weights of siblings are fixed before their work starts;
a parallel middle phase can therefore keep 30% of its parent's budget
regardless of worker count. The returned children can be shared across scoped
threads, then finished after their workers join.

`Pulse` extends `Stop` and `Report`, so the same `&dyn Pulse` also works
at existing `&dyn Stop` and `&dyn Report` seams on this crate's Rust 1.88 MSRV.
`Stop` and `StopReason` are re-exported here, so a new library needs only
`howfar` in its manifest.
`check()` does not report completed work, and `advance()` does not poll or call
subscribers. The caller chooses both cadences.

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

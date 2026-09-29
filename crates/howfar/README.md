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
use howfar::{Execution, Outcome, PhaseSpec, Pulse, Total};
use enough::StopReason;

fn process(rows: &[u8], pulse: &dyn Pulse) -> Result<(), StopReason> {
    pulse.check()?;
    let phases = pulse.split(Execution::Sequence, &[
        PhaseSpec::new("rows", 1, Total::Exact(rows.len() as u64)).units("rows"),
    ]).expect("this fixed plan is valid");
    let rows_phase = &*phases[0];
    for row in rows {
        // Process the row successfully, then count it.
        let _ = row;
        rows_phase.advance(1);
        rows_phase.check()?;
    }
    rows_phase.finish(Outcome::Succeeded).unwrap();
    pulse.finish(Outcome::Succeeded).unwrap();
    Ok(())
}

process(&[1, 2, 3], &howfar::NoPulse).unwrap();
```

A real library should propagate a failed `split`/`finish` through its own error
type and publish `Cancelled` or `Failed` outcomes on error. `NoPulse` is the
zero-sized, non-cancelling choice. Child phases can themselves split into
`Sequence`, `ForkJoin`, or `WorkPool` groups. The relative weights of siblings
are fixed before their work starts; a parallel middle phase can therefore keep
30% of its parent's budget regardless of worker count. The returned children
can be shared across scoped threads, then finished after their workers join.

`Pulse` extends `enough::Stop` and `Report`, so the same `&dyn Pulse` also works
at existing `&dyn Stop` and `&dyn Report` seams on this crate's Rust 1.88 MSRV.
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

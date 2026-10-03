# how-far

A small `no_std + alloc` interface for cooperative cancellation, completed work,
and weighted phases. Libraries accept `&dyn Pulse`; applications choose no-op,
cancellation-only, tree, or callback implementations. Requires pointer atomics,
Rust 1.86, and only the `enough` dependency. No clocks, macros or runtime. The optional `adapters` feature adds cancellation
wrappers without changing the interface.

```rust
use how_far::{PhaseSpec, ProgressExt, Pulse, RunError, Stages, StopReason, Total};
fn encode(rows: &[u8], pulse: &dyn Pulse) -> Result<(), RunError<StopReason>> {
    let mut stages = Stages::new(pulse, &[
        PhaseSpec::new("rows", 1, Total::Exact(rows.len() as u64)).units("rows"),
    ])?;
    stages.run_stoppable(|stage| {
        stage.check()?;
        let mut pace = stage.paced(64);
        for _row in rows {
            // Process the row successfully, then count it.
            pace.step(1)?;
        }
        pace.finish()
    })?;
    stages.finish()?;
    Ok(())
}
encode(&[1, 2, 3], &how_far::NoPulse)?;
# Ok::<(), RunError<StopReason>>(())
```

`check` polls cancellation and may run a checkpoint callback. `advance` records
completed work without calling application code. `step` counts then checks.
`Paced` batches reports per worker; `finish()` flushes the short final batch and
checks. Drop only flushes counts, including on an early return.

A borrowed pulse belongs to its caller. Finish only the `Child` owners returned
by your own splits. `Stages` starts and finishes its children, preserving work
errors; later stages prevented from running become `NotRun`, earning no progress.
Use `Skipped` only for intentionally unnecessary work.

`share()` returns an owned `SharedPulse` implementing the full `Pulse` interface,
so a spawned task can call another library and split its phase. Join it before
finishing the original child. Unsupported sharing returns `NotShareable`.
`handle()` remains the legacy count/check-only adapter; new code should use
`share()` when it needs to retain all capabilities.

`start()` marks active work without counting or preventing a split. `set_total()`
revises a leaf denominator; it never changes sibling weights. A nonzero report
prevents splitting; zero reports do nothing. Counters do not synchronize user data.

With `features = ["adapters"]`, `WithStop::stop_only(policy)` keeps cancellation through nested plans without tracking.
`WithStop::new(pulse, policy)` adds a policy through every split and shared handle.
`NoPulse` validates plan shapes but stores no lifecycle state. It can therefore
never diagnose repeated splits or reports after completion.

Tracking and callbacks live in `how-far-along`. Enable its `callback` feature for
`FnPulse`: callbacks run at checks, may inspect progress lazily, and may stop the
job. Thread-affine callbacks use its local observer poller. Neither callbacks nor
tracking are compiled into the core library dependency.

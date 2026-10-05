# how-far

A small `no_std + alloc` protocol for cooperative cancellation and approximate
progress. Libraries accept `&dyn Pulse` and return their own ordinary `Result`.
No tracker, clock, executor, serialization, or diagnostic collector is required.
Rust 1.86; pointer atomics and an allocator are required.

```rust
use how_far::{prelude::*, PhaseSpec, Stages, StopReason, Total};

fn encode(rows: &[u8], pulse: &dyn Pulse) -> Result<(), StopReason> {
    let mut stages = Stages::new(pulse, &[
        PhaseSpec::new("encode", 1, Total::Exact(rows.len() as u64)).units("rows"),
        PhaseSpec::new("optional metadata", 1, Total::Exact(1)),
    ]);
    let result = stages.run(|stage| {
        stage.check()?;
        let mut pace = stage.paced(64);
        for _row in rows {
            // Complete the row before reporting it.
            pace.step(1)?;
        }
        pace.finish()
    });
    result.finish_phase(stages)
}
encode(&[1, 2, 3], &how_far::NoPulse)?;
# Ok::<(), StopReason>(())
```

An application calls such a library with `&how_far::NoPulse`, a tracker from
`how-far-along`, or, when it has only a cancellation token, with
`&StopOnly::new(token)` (or `StopOnly::borrowed(&token)`): every nested phase
checks that token, and progress costs nothing.

`Stages` runs a sequence. An escaping error makes its remaining stages `NotRun`.
`Phases` runs independently selected phases, allowing a library to catch a failed
attempt and run a fallback. Neither helper introduces a planning error into the
work's result. Plans rejected by an observer retain cancellation/checkpoint
behavior through untracked children. `record_issue` lets an optional diagnostic
wrapper retain evidence of rejected observations.

`Complete::complete(result)` and `ResultExt::finish_phase(owner)` are explicit
result handoffs, returning the original value/error unchanged. Untouched phases
become `Skipped` on success or `NotRun` on failure. Drop without a result records
`Abandoned`. For a multi-step body, `owner.complete_with(|owner| ...)` hands
over whatever the body returns, so an early `?` cannot bypass the handoff. A borrowed pulse belongs to its caller: complete your child
owners and plan helpers, never the borrowed input.

For a custom error, implement `From<StopReason>` for `?` and
`TryFrom<&YourError> for StopReason` for cancellation classification. The latter
only borrows the error. Bare `StopReason` works directly. With a foreign error
wrapper that cannot implement the conversion, use an explicit classifier in
`run_classified`.

`check` invokes cancellation/checkpoint policy; `advance` only counts; `step`
counts then checks. One `Paced` per worker batches reports. Its `finish()` flushes
and checks the final partial batch. Its Drop only flushes; it cannot return a
cancellation error. Counters do not synchronize application data.

`share()` preserves the complete Pulse interface for owned tasks but grants no
completion rights. Join workers before completing their original owner.
Unsupported sharing is an explicit `NotShareable` error, never a silent loss of
cancellation. `Pulse::split`, `start`, `set_total`,
and `Child::finish` are the explicitly fallible administration layer. The optional
`checked` feature adds `TryStages` and its `RunError` wrapper.

The optional `adapters` feature adds `WithStop`, preserving an additional stop
policy through children and owned views. `WithStop::borrowed(pulse, stop)`
combines a caller's pulse with a local `&dyn Stop`; `replacing_borrowed` uses only
the new policy (bypassing the inner check and its callbacks). Borrowed policies
work with scoped workers; owned `new`/`replacing` policies can use `share()`. Tracking and lazy callbacks live in
`how-far-along`; profiling and diagnostics live in `how-far-really`.

Runnable, cross-crate usage and intentional mistakes are in
[the example crates](../../examples/how-far-app/README.md).

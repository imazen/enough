# howfar

A tiny interface for reporting completed work. **Zero dependencies, no feature
flags, `no_std + alloc`, and `#![forbid(unsafe_code)]`.** Rust 1.88+.

Library authors depend on `howfar`. Applications and library tests can opt into
[`howfar-along`](../howfar-along/README.md) for counters, weighted phases,
callbacks, snapshots, and profiling. A library's users do not compile that
machinery just because the library supports progress.

```toml
[dependencies]
howfar = "0.1"
```

```rust
use howfar::{IgnoreProgress, Report};

pub fn process(rows: &[u8], progress: impl Report) {
    for chunk in rows.chunks(16) {
        // Successfully process the chunk, then report actual completed rows.
        progress.advance(chunk.len() as u64);
    }
}

process(&[0; 17], IgnoreProgress);
```

`IgnoreProgress` deliberately discards reports; it does not mean the operation
has made no progress. It is zero-sized and optimizes away in generic code.

A caller can implement `Report` with a borrowed atomic counter, a custom metric,
or a test recorder. Workers can share the sink: `Report` requires `Send + Sync`.
`advance` counts completed units; it does not imply a clock read, callback,
cancellation check, or percentage calculation. Check `may_report()` when expensive
report preparation could be skipped for a permanent no-op sink.

References, `Box`, `Arc`, and `Option` have explicit forwarding implementations.
They are always available: `alloc` is required, with no feature switch or
macro-generated implementation. The API is identical in every build. `Report`
and `IgnoreProgress` themselves never allocate; the caller chooses its storage.

## Cancellation stays independent

Use `enough::Stop` alongside `Report` when a library supports cancellation:

```rust,ignore
use enough::{Stop, StopReason};
use howfar::Report;

pub fn process(rows: &[u8], stop: impl Stop, progress: impl Report)
    -> Result<(), StopReason>
{
    stop.check()?;
    for chunk in rows.chunks(16) {
        // Process the chunk successfully.
        progress.advance(chunk.len() as u64);
        stop.check()?;
    }
    Ok(())
}
```

Keep the initial check, count the actual final partial batch, and choose checking
and reporting cadence separately. An API may also accept `impl Stop + Report`;
consumers can supply `howfar_along::Work` to combine their two policies.

## Opt into tracking only where it is used

```toml
# In a library's Cargo.toml:
[dependencies]
howfar = "0.1"
enough = "0.4"

[dev-dependencies]
howfar-along = { version = "0.1", features = ["profile"] }
```

Applications can put `howfar-along` in normal dependencies instead. The tracker
has the same tree/polling API with std or no_std + alloc; it owns synchronization
and instrumentation costs. `howfar` always remains the lightweight interface.
Neither crate contains unsafe code or proc-macro dependencies.

Fresh-target default library builds on the development host measured **0.088 s
for howfar versus 0.099 s for enough** (five runs, medians, warm toolchain and
filesystem caches). This is a host measurement, not an absolute CI time limit.
See [the validation notes](../../docs/howfar-implementation.md).

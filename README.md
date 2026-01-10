# enough

Minimal cooperative cancellation for Rust.

[![Crates.io](https://img.shields.io/crates/v/enough.svg)](https://crates.io/crates/enough)
[![Documentation](https://docs.rs/enough/badge.svg)](https://docs.rs/enough)
[![License](https://img.shields.io/crates/l/enough.svg)](LICENSE-MIT)
[![MSRV](https://img.shields.io/badge/MSRV-1.56-blue.svg)](https://blog.rust-lang.org/2021/10/21/Rust-1.56.0.html)

## Why "enough"?

Sometimes you've had *enough*. The operation is taking too long, the user hit cancel, or resources are constrained. This crate provides a minimal, shared trait that libraries can use to support cooperative cancellation without heavy dependencies.

## Design Rationale

**Problem:** Image codecs, compression libraries, and other CPU-intensive operations need cancellation support, but shouldn't dictate which cancellation system you use.

**Solution:** A minimal `Stop` trait that any cancellation system can implement:

```rust
pub trait Stop: Send + Sync {
    fn check(&self) -> Result<(), StopReason>;
    fn should_stop(&self) -> bool { self.check().is_err() }
}
```

**Key decisions:**
- **`no_std` core** - Works in embedded, WASM, everywhere
- **Zero dependencies** - Won't bloat your dependency tree
- **Bring your own impl** - Works with tokio, custom systems, FFI
- **Error propagation via `?`** - Integrates cleanly with Result chains

## Quick Start

### For Library Authors

Accept `impl Stop` - that's it:

```rust
use enough::{Stop, StopReason};

pub fn process(data: &[u8], stop: impl Stop) -> Result<Vec<u8>, MyError> {
    let mut output = Vec::new();
    for (i, chunk) in data.chunks(1024).enumerate() {
        if i % 16 == 0 {
            stop.check()?;  // Returns Err(StopReason) if stopped
        }
        // ... process chunk ...
    }
    Ok(output)
}

impl From<StopReason> for MyError {
    fn from(r: StopReason) -> Self { MyError::Stopped(r) }
}
```

### For Application Developers

Choose the implementation that fits your needs:

```rust
use enough::{ArcStop, Stop};
use std::time::Duration;

// Create a cancellation source
let source = ArcStop::new();
let token = source.token();

// Pass to libraries
let handle = std::thread::spawn(move || {
    my_codec::process(&data, token)
});

// Cancel when needed
source.cancel();
```

### Zero-Cost When Not Needed

```rust
use enough::Never;

// Compiles to nothing - zero runtime cost
let result = my_codec::process(&data, Never);
```

## Type Overview

| Type | Feature | Use Case |
|------|---------|----------|
| `Never` | core | Zero-cost "never stop" |
| `AtomicStop` / `AtomicToken` | core | Stack-based, borrowed, Relaxed ordering |
| `SyncStop` / `SyncToken` | core | Stack-based, Acquire/Release for data sync |
| `FnStop` | core | Wrap any closure |
| `OrStop` | core | Combine multiple stop sources |
| `ArcStop` / `ArcToken` | alloc | Heap, owned tokens can outlive source |
| `BoxStop` | alloc | Type-erased dynamic dispatch |
| `ChildSource` / `ChildToken` | alloc | Hierarchical (parent cancels children) |
| `WithTimeout` | std | Add deadline to any Stop |

## Feature Flags

```toml
[dependencies]
enough = "0.1"                    # no_std core only
enough = { version = "0.1", features = ["alloc"] }  # + Arc types
enough = { version = "0.1", features = ["std"] }    # + timeouts (implies alloc)
```

## Memory Ordering

Two variants for different needs:

```rust
use enough::{AtomicStop, SyncStop};

// AtomicStop: Relaxed ordering (faster on ARM)
// Use when you just need to signal "stop"
let stop = AtomicStop::new();
stop.cancel();  // Relaxed store
stop.should_stop();  // Relaxed load

// SyncStop: Release/Acquire ordering
// Use when stop signals data is ready
let stop = SyncStop::new();
// Thread A:
shared_result.store(42, Relaxed);
stop.cancel();  // Release: flushes shared_result

// Thread B:
if stop.should_stop() {  // Acquire: syncs with Release
    shared_result.load(Relaxed);  // Guaranteed to see 42
}
```

## Common Patterns

### Timeouts

```rust
use enough::{ArcStop, TimeoutExt};
use std::time::Duration;

let source = ArcStop::new();
let token = source.token()
    .with_timeout(Duration::from_secs(30));

// Stops if cancelled OR timeout expires
```

### Hierarchical Cancellation

```rust
use enough::{ArcStop, children::ChildSource};

let parent = ArcStop::new();
let child_a = ChildSource::new(parent.token());
let child_b = ChildSource::new(parent.token());

child_a.cancel();  // Only child_a stops
parent.cancel();   // Both children stop
```

### Combining Sources

```rust
use enough::{ArcStop, OrStop};

let app_cancel = ArcStop::new();
let timeout = ArcStop::new();

// Stop if either triggers
let combined = OrStop::new(app_cancel.token(), timeout.token());
```

### With Tokio

```rust
use enough_tokio::TokioStop;
use tokio_util::sync::CancellationToken;

let token = CancellationToken::new();
let stop = TokioStop::new(token.clone());

tokio::task::spawn_blocking(move || {
    my_codec::process(&data, stop)
});
```

## Related Crates

| Crate | Purpose |
|-------|---------|
| `enough` | Core trait + implementations |
| `enough-ffi` | C FFI for cross-language use |
| `enough-tokio` | Bridge to tokio's CancellationToken |
| `almost-enough` | Ergonomic extensions (`.or()`, `.into_boxed()`, `StopDropRoll`) |

## Performance

| Operation | Time | Notes |
|-----------|------|-------|
| `Never.check()` | 0ns | Optimized away |
| `AtomicToken.check()` | ~1-2ns | Single atomic load |
| `WithTimeout.check()` | ~20-30ns | Includes `Instant::now()` |

Check every 16-100 iterations for negligible overhead.

## Adaptability

The trait-based design means you can:

1. **Start simple** - Use `Never` during development
2. **Add cancellation** - Switch to `ArcStop` when needed
3. **Add timeouts** - Wrap with `.with_timeout()`
4. **Go hierarchical** - Use `ChildSource` for complex flows
5. **Integrate with async** - Use `enough-tokio`
6. **Call from FFI** - Use `enough-ffi`

Libraries accepting `impl Stop` work with all of these without changes.

## License

Licensed under either of Apache License 2.0 or MIT license at your option.

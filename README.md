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
- **1-byte error type** - `StopReason` compiles to a single boolean read from the stack

## Crate Structure

| Crate | Purpose |
|-------|---------|
| `enough` | Core trait only: `Stop`, `StopReason`, `Unstoppable` |
| `almost-enough` | **All implementations**: `Stopper`, `StopSource`, timeouts, combinators |
| `enough-ffi` | C FFI for cross-language use |
| `enough-tokio` | Bridge to tokio's CancellationToken |

## Quick Start

### For Library Authors

Depend on `enough` (minimal) and accept `impl Stop`:

```rust
use enough::{Stop, StopReason, Unstoppable};

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

// Re-export for caller convenience
pub use enough::Unstoppable;
```

**Naming conventions:**

For **new libraries**, accept `impl Stop` (generic, not `dyn`) as the final parameter with no suffix. Re-export `Unstoppable` so callers can easily opt out of cancellation:

```rust
// Caller uses your re-export
use my_codec::{process, Unstoppable};
let result = process(&data, Unstoppable);
```

For **existing libraries** adding cancellation support, create a `_stoppable` variant and delegate:

```rust
// Original function - unchanged API
pub fn decode(data: &[u8]) -> Result<Image, Error> {
    decode_stoppable(data, Unstoppable)
}

// New stoppable variant
pub fn decode_stoppable(data: &[u8], stop: impl Stop) -> Result<Image, Error> {
    // ... implementation with stop.check() calls
}
```

This preserves backwards compatibility while making cancellation available.

### For Application Developers

```toml
[dependencies]
almost-enough = "0.1"  # Includes all implementations
```

Choose the implementation that fits your needs:

```rust
use almost_enough::{Stopper, Stop};

// Create a cancellation source - clone to share
let stop = Stopper::new();
let stop2 = stop.clone();

// Pass to libraries
let handle = std::thread::spawn(move || {
    my_codec::process(&data, stop2)
});

// Any clone can cancel
stop.cancel();
```

### Zero-Cost When Not Needed

```rust
use almost_enough::Unstoppable;  // or enough::Unstoppable

// Compiles to nothing - zero runtime cost
let result = my_codec::process(&data, Unstoppable);
```

## Type Overview

| Type | Crate | Feature | Use Case |
|------|-------|---------|----------|
| `Stop` | enough | core | The trait |
| `StopReason` | enough | core | Cancellation reason enum |
| `Unstoppable` | enough | core | Zero-cost "never stop" |
| `StopSource` / `StopRef` | almost-enough | core | Stack-based, borrowed, Relaxed ordering |
| `FnStop` | almost-enough | core | Wrap any closure |
| `OrStop` | almost-enough | core | Combine multiple stop sources |
| `Stopper` | almost-enough | alloc | **Default choice** - Arc-based, clone to share |
| `SyncStopper` | almost-enough | alloc | Like Stopper with Acquire/Release ordering |
| `ChildStopper` | almost-enough | alloc | Hierarchical parent-child cancellation |
| `BoxedStop` | almost-enough | alloc | Type-erased dynamic dispatch |
| `WithTimeout` | almost-enough | std | Add deadline to any Stop |

## Feature Flags

**`enough`** (for library authors):
```toml
enough = "0.1"                    # no_std core only
enough = { version = "0.1", features = ["alloc"] }  # + Box/Arc impls
enough = { version = "0.1", features = ["std"] }    # + Error impl
```

**`almost-enough`** (for applications):
```toml
almost-enough = "0.1"                    # std (default) - all features
almost-enough = { version = "0.1", default-features = false, features = ["alloc"] }  # no_std + alloc
```

## Memory Ordering

Two variants for different needs:

```rust
use almost_enough::{Stopper, SyncStopper};

// Stopper: Relaxed ordering (faster on ARM)
// Use when you just need to signal "stop"
let stop = Stopper::new();
stop.cancel();  // Relaxed store
stop.should_stop();  // Relaxed load

// SyncStopper: Release/Acquire ordering
// Use when stop signals data is ready
let stop = SyncStopper::new();
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
use almost_enough::{Stopper, TimeoutExt};
use std::time::Duration;

let stop = Stopper::new();
let timed = stop.clone().with_timeout(Duration::from_secs(30));

// Stops if cancelled OR timeout expires
```

### Hierarchical Cancellation

```rust
use almost_enough::ChildStopper;

let parent = ChildStopper::new();
let child_a = parent.child();
let child_b = parent.child();

child_a.cancel();  // Only child_a stops
parent.cancel();   // Both children stop
```

### Combining Sources

```rust
use almost_enough::{Stopper, StopExt};

let app_cancel = Stopper::new();
let timeout = Stopper::new();

// Stop if either triggers
let combined = app_cancel.clone().or(timeout.clone());
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

## Performance

| Operation | Time | Notes |
|-----------|------|-------|
| `Unstoppable.check()` | 0ns | Optimized away |
| `Stopper.check()` | ~1-2ns | Single atomic load |
| `WithTimeout.check()` | ~20-30ns | Includes `Instant::now()` |

Check every 16-100 iterations for negligible overhead.

## Adaptability

The trait-based design means you can:

1. **Start simple** - Use `Unstoppable` during development
2. **Add cancellation** - Switch to `Stopper` when needed
3. **Add timeouts** - Wrap with `.with_timeout()`
4. **Go hierarchical** - Use `ChildStopper` for complex flows
5. **Integrate with async** - Use `enough-tokio`
6. **Call from FFI** - Use `enough-ffi`

Libraries accepting `impl Stop` work with all of these without changes.

## Zero-Cost Proof

`impl Stop` is generic, not `dyn` - each type is monomorphized. When you pass `Unstoppable`, the compiler eliminates all cancellation checks entirely.

```rust
#[inline(never)]
pub fn process<S: Stop>(data: &[u8], stop: S) -> usize {
    let mut sum = 0usize;
    for (i, &byte) in data.iter().enumerate() {
        if i % 16 == 0 && stop.should_stop() {  // <-- eliminated for Unstoppable
            return sum;
        }
        sum = sum.wrapping_add(byte as usize);
    }
    sum
}
```

Verify with `cargo asm`:

```x86asm
asm_demo::process_with_stop:            # Monomorphized for Unstoppable
        mov     ecx, 4
        xor     eax, eax
.LBB4_1:
        movzx   edx, byte ptr [rdi + rcx - 4]
        add     rdx, rax
        movzx   eax, byte ptr [rdi + rcx - 3]
        ; ... unrolled loop, NO cancellation check
        add     rcx, 5
        cmp     rcx, 104
        jne     .LBB4_1
        ret
```

No atomic loads, no branches for cancellation - just the loop. The `if stop.should_stop()` branch is dead-code eliminated because `Unstoppable::should_stop()` is `#[inline(always)]` and returns `false`.

## License

Licensed under either of Apache License 2.0 or MIT license at your option.

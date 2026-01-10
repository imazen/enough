# enough

Minimal cooperative cancellation for Rust.

[![Crates.io](https://img.shields.io/crates/v/enough.svg)](https://crates.io/crates/enough)
[![Documentation](https://docs.rs/enough/badge.svg)](https://docs.rs/enough)
[![License](https://img.shields.io/crates/l/enough.svg)](LICENSE-MIT)
[![MSRV](https://img.shields.io/badge/MSRV-1.56-blue.svg)](https://blog.rust-lang.org/2021/10/21/Rust-1.56.0.html)

## Why "enough"?

Sometimes you've had *enough*. The operation is taking too long, the user hit cancel, or resources are constrained. This crate provides a minimal, shared trait that libraries can use to support cooperative cancellation without taking on heavy dependencies.

## Features

- **`no_std` core** - The `Stop` trait works anywhere, no allocator required
- **Zero dependencies** - Core trait has no dependencies at all
- **Zero-cost `Never`** - Optimized away completely when cancellation not needed
- **Minimal API** - Just `check()` and `is_stopped()`, nothing more
- **Timeout support** - Deadlines that only tighten, never loosen
- **Hierarchical cancellation** - Child sources inherit parent cancellation
- **FFI ready** - Bridge to C#, Python, Node.js via `enough-ffi`

## Quick Start

### For Library Authors

Accept `impl Stop` in long-running functions. This is the only thing you need from this crate:

```rust
use enough::{Stop, StopReason};

pub fn decode(data: &[u8], stop: impl Stop) -> Result<Vec<u8>, DecodeError> {
    let mut output = Vec::new();
    for (i, chunk) in data.chunks(1024).enumerate() {
        // Check periodically - every 16-100 iterations is typical
        if i % 16 == 0 {
            stop.check()?;
        }
        // process chunk...
        output.extend_from_slice(chunk);
    }
    Ok(output)
}

// One-line error integration
#[derive(Debug)]
pub enum DecodeError {
    Stopped(StopReason),
    // ... other errors
}

impl From<StopReason> for DecodeError {
    fn from(r: StopReason) -> Self { DecodeError::Stopped(r) }
}
```

### For Application Developers

Enable the `std` feature for concrete implementations:

```rust
use enough::{CancellationSource, Stop};
use std::time::Duration;

// Create a cancellation source
let source = CancellationSource::new();

// Get a token with optional timeout
let token = source.token().with_timeout(Duration::from_secs(30));

// Pass to library functions
let result = my_codec::decode(&data, token);

// Or cancel programmatically
source.cancel();
```

### Zero-Cost Default

When callers don't need cancellation, use `Never`:

```rust
use enough::Never;

// Compiles to nothing - zero runtime cost
let result = my_codec::decode(&data, Never);
```

## Crates

| Crate | Description | Features |
|-------|-------------|----------|
| [`enough`](https://crates.io/crates/enough) | Core `Stop` trait | `no_std`, zero deps |
| [`enough`](https://crates.io/crates/enough) with `std` | `CancellationSource`, timeouts, child cancellation | Requires `std` |
| [`enough-ffi`](https://crates.io/crates/enough-ffi) | C-compatible FFI for cross-language use | C#, Python, Node.js |
| [`enough-tokio`](https://crates.io/crates/enough-tokio) | Bridge to `tokio_util::sync::CancellationToken` | Async runtimes |

## Feature Flags

The `enough` crate has these features:

- **`std`** (default: off) - Enables `CancellationSource`, `CancellationToken`, timeouts, child cancellation, and `std::error::Error` impl
- **`alloc`** (default: off) - Enables blanket `Stop` impls for `Box<T>` and `Arc<T>`

## Usage Patterns

### Timeout That Only Tightens

Timeouts compose safely - child timeouts can only be stricter:

```rust
use enough::CancellationSource;
use std::time::Duration;

let source = CancellationSource::new();

// Parent operation: 60 seconds
let parent_token = source.token().with_timeout(Duration::from_secs(60));

// Sub-operation: wants 10 seconds, but will respect parent's remaining time
let step_token = parent_token.clone().with_timeout(Duration::from_secs(10));
// Effective: min(parent_remaining, 10s)
```

### Hierarchical Cancellation

Create cancellation trees where children inherit parent cancellation:

```rust
use enough::{CancellationSource, ChildCancellationSource};

let parent = CancellationSource::new();
let child_a = ChildCancellationSource::new(parent.token());
let child_b = ChildCancellationSource::new(parent.token());

// Cancel child_a only - child_b continues
child_a.cancel();
assert!(child_a.is_cancelled());
assert!(!child_b.is_cancelled());

// Cancel parent - all children stop
parent.cancel();
assert!(child_b.is_cancelled());
```

### With Tokio

Bridge to async cancellation:

```rust
use enough_tokio::TokioStop;
use tokio_util::sync::CancellationToken;

let token = CancellationToken::new();
let stop = TokioStop::new(token.clone());

tokio::task::spawn_blocking(move || {
    // Use stop with any library that accepts impl Stop
    my_codec::decode(&data, stop)
});
```

### FFI Integration

#### C# / .NET

```csharp
// P/Invoke declarations
[DllImport("mylib")]
static extern IntPtr enough_cancellation_create();

[DllImport("mylib")]
static extern void enough_cancellation_cancel(IntPtr source);

[DllImport("mylib")]
static extern IntPtr enough_token_create(IntPtr source);

[DllImport("mylib")]
static extern bool enough_token_is_cancelled(IntPtr token);

[DllImport("mylib")]
static extern void enough_token_destroy(IntPtr token);

[DllImport("mylib")]
static extern void enough_cancellation_destroy(IntPtr source);

// Usage with .NET CancellationToken
public byte[] Decode(byte[] data, CancellationToken ct)
{
    var source = enough_cancellation_create();
    var token = enough_token_create(source);
    try
    {
        using var reg = ct.Register(() => enough_cancellation_cancel(source));
        return NativeMethods.decode(data, token);
    }
    finally
    {
        enough_token_destroy(token);
        enough_cancellation_destroy(source);
    }
}
```

#### Rust FFI Functions

```rust
use enough_ffi::FfiCancellationToken;
use enough::Stop;

#[no_mangle]
pub extern "C" fn decode(
    data: *const u8,
    len: usize,
    token: *const FfiCancellationToken,
) -> i32 {
    // Create a view from the pointer - no ownership transfer
    let stop = unsafe { FfiCancellationToken::from_ptr(token) };

    // Use with any library accepting impl Stop
    match my_codec::decode(unsafe { std::slice::from_raw_parts(data, len) }, stop) {
        Ok(_) => 0,
        Err(e) if e.is_stopped() => -1,
        Err(_) => -2,
    }
}
```

## Performance

| Operation | Time | Notes |
|-----------|------|-------|
| `Never.check()` | 0ns | Optimized away entirely |
| `CancellationToken.check()` | ~1ns | Single atomic load |
| `token.with_timeout().check()` | ~20-30ns | Includes `Instant::now()` |

**Recommendation:** Check every 16-100 iterations for negligible overhead on real workloads.

## API Summary

### Core (`no_std`)

```rust
// The trait libraries should accept
pub trait Stop: Send + Sync {
    fn check(&self) -> Result<(), StopReason>;
    fn is_stopped(&self) -> bool;
}

// Zero-cost "never stop" implementation
pub struct Never;

// Why the operation stopped
#[non_exhaustive]
pub enum StopReason {
    Cancelled,
    TimedOut,
}
```

### With `std` Feature

```rust
// Create and control cancellation
pub struct CancellationSource { /* ... */ }
impl CancellationSource {
    pub fn new() -> Self;
    pub fn cancel(&self);
    pub fn is_cancelled(&self) -> bool;
    pub fn token(&self) -> CancellationToken;
}

// Pass to operations
pub struct CancellationToken { /* ... */ }
impl CancellationToken {
    pub fn never() -> Self;
    pub fn with_timeout(self, duration: Duration) -> Self;
    pub fn with_deadline(self, deadline: Instant) -> Self;
}
impl Stop for CancellationToken { /* ... */ }

// Hierarchical cancellation
pub struct ChildCancellationSource { /* ... */ }
pub struct ChildCancellationToken { /* ... */ }
```

## Design Philosophy

1. **Minimal surface** - Only what's needed, nothing more
2. **Zero-cost abstraction** - `Never` compiles away completely
3. **No runtime dependencies** - Core trait is dependency-free
4. **Cooperative, not preemptive** - Libraries check when convenient
5. **Composable** - Timeouts tighten, cancellation inherits

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

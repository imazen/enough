# enough

Cooperative cancellation for Rust - a minimal, `no_std` trait for long-running operations.

[![Crates.io](https://img.shields.io/crates/v/enough.svg)](https://crates.io/crates/enough)
[![Documentation](https://docs.rs/enough/badge.svg)](https://docs.rs/enough)
[![License](https://img.shields.io/crates/l/enough.svg)](LICENSE-MIT)

## Why "enough"?

Sometimes you've had *enough* - the operation is taking too long, the user cancelled, or you just need to stop. This crate provides a minimal shared trait that libraries can accept to support cooperative cancellation.

## Quick Start

### For Library Authors

Accept `impl Stop` in your long-running functions:

```rust
use enough::{Stop, StopReason};

pub fn decode(data: &[u8], stop: impl Stop) -> Result<Vec<u8>, DecodeError> {
    for (i, chunk) in data.chunks(1024).enumerate() {
        // Check periodically
        if i % 16 == 0 {
            stop.check()?;
        }
        // process chunk...
    }
    Ok(output)
}

// One-line error integration
impl From<StopReason> for DecodeError {
    fn from(r: StopReason) -> Self { DecodeError::Stopped(r) }
}
```

### For Application Developers

```rust
use enough_std::{CancellationSource, CancellationToken};
use std::time::Duration;

// Create a source
let source = CancellationSource::new();

// Get a token with timeout
let token = source.token().with_timeout(Duration::from_secs(30));

// Pass to library
let result = my_codec::decode(&data, token);

// Or cancel manually
source.cancel();
```

## Crates

| Crate | Description |
|-------|-------------|
| [`enough`](crates/enough) | Core trait, `no_std`, zero dependencies |
| [`enough-std`](crates/enough-std) | Standard implementations (`CancellationSource`, timeouts) |
| [`enough-tokio`](crates/enough-tokio) | Tokio `CancellationToken` bridge |
| [`enough-ffi`](crates/enough-ffi) | FFI helpers for C#/.NET integration |

## Features

- **`no_std` core** - The `Stop` trait works anywhere
- **Zero-cost `Never`** - Compiles to nothing when cancellation not needed
- **Copy tokens** - No lifetime hassles, pass freely
- **Timeout tightening** - Child timeouts can only be stricter
- **Child cancellation** - Hierarchical cancellation trees
- **FFI ready** - Bridge to C#'s `CancellationToken`

## Usage Patterns

### With Timeout

```rust
let token = source.token()
    .with_timeout(Duration::from_secs(30));
```

### Chaining (Tightens, Never Loosens)

```rust
// Parent: 60s, child step: 10s
let step_token = parent_token.with_timeout(Duration::from_secs(10));
// Effective timeout: min(remaining_parent, 10s)
```

### Child Cancellation

```rust
use enough_std::{CancellationSource, ChildCancellationSource};

let parent = CancellationSource::new();
let child_a = ChildCancellationSource::new(parent.token());
let child_b = ChildCancellationSource::new(parent.token());

// Cancel child_a only - child_b continues
child_a.cancel();

// Cancel parent - all children stop
parent.cancel();
```

### With Tokio

```rust
use enough_tokio::TokioStop;

let token = tokio_util::sync::CancellationToken::new();
let stop = TokioStop::new(token.clone());

tokio::task::spawn_blocking(move || {
    my_codec::decode(&data, stop)
});
```

### With FFI (C#)

```csharp
var handle = NativeMethods.enough_cancellation_create();
try
{
    // Bridge .NET CancellationToken to Rust
    using var registration = cancellationToken.Register(() =>
        NativeMethods.enough_cancellation_cancel(handle));

    return NativeMethods.decode(data, handle);
}
finally
{
    NativeMethods.enough_cancellation_destroy(handle);
}
```

## Performance

| Operation | Time |
|-----------|------|
| `Never.check()` | 0 (optimized away) |
| `CancellationToken.check()` | ~1ns (atomic load) |
| With deadline check | ~20-30ns (includes `Instant::now()`) |

Check every 16-100 iterations for negligible overhead on real workloads.

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

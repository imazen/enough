# enough

Minimal cooperative cancellation for Rust.

[![Crates.io](https://img.shields.io/crates/v/enough.svg)](https://crates.io/crates/enough)
[![Documentation](https://docs.rs/enough/badge.svg)](https://docs.rs/enough)
[![License](https://img.shields.io/crates/l/enough.svg)](LICENSE-MIT)
[![MSRV](https://img.shields.io/badge/MSRV-1.56-blue.svg)](https://blog.rust-lang.org/2021/10/21/Rust-1.56.0.html)

A minimal, `no_std` trait for cooperative cancellation in long-running operations. Zero dependencies in the core crate.

## For Library Authors

Accept `impl Stop` in your functions:

```rust
use enough::{Stop, StopReason};

pub fn decode(data: &[u8], stop: impl Stop) -> Result<Vec<u8>, MyError> {
    for (i, chunk) in data.chunks(1024).enumerate() {
        if i % 16 == 0 {
            stop.check()?; // Returns Err(StopReason) if stopped
        }
        // process...
    }
    Ok(vec![])
}

impl From<StopReason> for MyError {
    fn from(r: StopReason) -> Self { MyError::Stopped(r) }
}
```

## For Application Developers

Enable the `std` feature:

```toml
[dependencies]
enough = { version = "0.1", features = ["std"] }
```

```rust
use enough::{CancellationSource, Stop};
use std::time::Duration;

let source = CancellationSource::new();
let token = source.token().with_timeout(Duration::from_secs(30));

// Pass to library
let result = my_lib::decode(&data, token);

// Or cancel manually
source.cancel();
```

## Zero-Cost Default

```rust
use enough::Never;

// Compiles away completely - zero runtime cost
let result = my_lib::decode(&data, Never);
```

## Features

- **`std`** - Enables `CancellationSource`, timeouts, child cancellation
- **`alloc`** - Enables `Stop` impls for `Box<T>` and `Arc<T>`

## See Also

- [`enough-ffi`](https://crates.io/crates/enough-ffi) - FFI helpers for C#, Python, Node.js
- [`enough-tokio`](https://crates.io/crates/enough-tokio) - Tokio integration

## License

MIT OR Apache-2.0

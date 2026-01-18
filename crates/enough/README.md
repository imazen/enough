# enough

Minimal cooperative cancellation trait for Rust.

[![CI](https://github.com/imazen/enough/actions/workflows/ci.yml/badge.svg)](https://github.com/imazen/enough/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/enough.svg)](https://crates.io/crates/enough)
[![Documentation](https://docs.rs/enough/badge.svg)](https://docs.rs/enough)
[![codecov](https://codecov.io/gh/imazen/enough/graph/badge.svg)](https://codecov.io/gh/imazen/enough)
[![License](https://img.shields.io/crates/l/enough.svg)](LICENSE-MIT)
[![MSRV](https://img.shields.io/badge/MSRV-1.56-blue.svg)](https://blog.rust-lang.org/2021/10/21/Rust-1.56.0.html)

A minimal, `no_std` trait for cooperative cancellation. Zero dependencies.

`StopReason` is 1 byte and `check()` compiles to a single boolean read from the stack.

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

## Zero-Cost Default

```rust
use enough::Unstoppable;

// Compiles away completely - zero runtime cost
let result = my_lib::decode(&data, Unstoppable);
```

## What's in This Crate

This crate provides only the **core trait and types**:

- `Stop` - The cooperative cancellation trait
- `StopReason` - Why an operation stopped (Cancelled or TimedOut)
- `Unstoppable` - Zero-cost "never stop" implementation

For concrete cancellation implementations (`Stopper`, `StopSource`, timeouts, etc.), see [`almost-enough`](https://crates.io/crates/almost-enough).

## Features

- **None (default)** - `no_std` core: `Stop` trait, `StopReason`, `Unstoppable`
- **`alloc`** - Adds `Box<T>` and `Arc<T>` blanket impls for `Stop`
- **`std`** - Implies `alloc`. Adds `std::error::Error` impl for `StopReason`

## See Also

- [`almost-enough`](https://crates.io/crates/almost-enough) - **All implementations**: `Stopper`, `StopSource`, `ChildStopper`, timeouts, combinators, guards
- [`enough-ffi`](https://crates.io/crates/enough-ffi) - FFI helpers for C#, Python, Node.js
- [`enough-tokio`](https://crates.io/crates/enough-tokio) - Tokio CancellationToken bridge

## License

MIT OR Apache-2.0

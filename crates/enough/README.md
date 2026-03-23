# enough

Minimal cooperative cancellation trait for Rust.

[![CI](https://github.com/imazen/enough/actions/workflows/ci.yml/badge.svg)](https://github.com/imazen/enough/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/enough.svg?style=for-the-badge)](https://crates.io/crates/enough)
[![Documentation](https://docs.rs/enough/badge.svg?style=for-the-badge)](https://docs.rs/enough)
[![codecov](https://codecov.io/gh/imazen/enough/graph/badge.svg)](https://codecov.io/gh/imazen/enough)
[![License](https://img.shields.io/crates/l/enough.svg?style=for-the-badge)](LICENSE-MIT)
[![MSRV](https://img.shields.io/badge/MSRV-1.89-blue.svg?style=for-the-badge)](https://blog.rust-lang.org/2025/05/15/Rust-1.89.0.html)

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

## Optimizing Hot Loops with `dyn Stop`

Behind `&dyn Stop`, the compiler can't inline away `Unstoppable::check()`. Use `may_stop()` with `Option<T>` to eliminate that overhead:

```rust
use enough::{Stop, StopReason};

fn process(stop: &dyn Stop) -> Result<(), StopReason> {
    let stop = stop.may_stop().then_some(stop); // Option<&dyn Stop>
    for i in 0..1_000_000 {
        stop.check()?; // None → Ok(()), Some → one vtable dispatch
    }
    Ok(())
}
```

`Option<T: Stop>` implements `Stop`: `None` is a no-op, `Some(inner)` delegates. The branch predictor handles the constant `None`/`Some` perfectly.

## What's in This Crate

This crate provides only the **core trait and types**:

- `Stop` - The cooperative cancellation trait
- `StopReason` - Why an operation stopped (Cancelled or TimedOut)
- `Unstoppable` - Zero-cost "never stop" implementation
- `impl Stop for Option<T: Stop>` - No-op when `None`, delegates when `Some`

For concrete cancellation implementations (`Stopper`, `StopSource`, timeouts, etc.), see [`almost-enough`](https://crates.io/crates/almost-enough).

## Features

- **None (default)** - `no_std` core: `Stop` trait, `StopReason`, `Unstoppable`
- **`alloc`** - Adds `Box<T>` and `Arc<T>` blanket impls for `Stop`
- **`std`** - Implies `alloc` (kept for downstream compatibility)

## See Also

- [`almost-enough`](https://crates.io/crates/almost-enough) - **All implementations**: `Stopper`, `StopSource`, `ChildStopper`, timeouts, combinators, guards
- [`enough-ffi`](https://crates.io/crates/enough-ffi) - FFI helpers for C#, Python, Node.js
- [`enough-tokio`](https://crates.io/crates/enough-tokio) - Tokio CancellationToken bridge

## License

MIT OR Apache-2.0

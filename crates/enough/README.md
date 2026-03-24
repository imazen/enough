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

## Choosing a Function Signature

| Signature | Clone | None-able | Monomorphized | Dep |
|-----------|:-----:|:---------:|:-------------:|-----|
| `Option<&dyn Stop>` | no | **yes** | no | enough |
| `impl CloneStop` | **yes** | no | yes | enough |
| `&dyn Stop` | no | no | no | enough |
| [`DynStop`] | **yes** | no | no | almost-enough |

[`DynStop`]: https://docs.rs/almost-enough/latest/almost_enough/struct.DynStop.html

### Simple APIs: `Option<&dyn Stop>`

For functions that just check periodically, `Option<&dyn Stop>` accepts
every stop type. Callers pass `None` for no cancellation — zero overhead,
no imports needed:

```rust
use enough::{Stop, StopReason};

pub fn decode(data: &[u8], stop: Option<&dyn Stop>) -> Result<Vec<u8>, MyError> {
    for (i, chunk) in data.chunks(1024).enumerate() {
        if i % 16 == 0 {
            stop.check()?; // None → Ok(()), Some → one dispatch
        }
        // process...
    }
    Ok(vec![])
}

// Callers:
decode(&data, None)?;              // no cancellation — zero overhead
decode(&data, Some(&stopper))?;    // with cancellation
```

### Parallel APIs: `impl CloneStop`

Functions that spawn threads need `Clone`. Use `CloneStop` (a trait alias
for `Stop + Clone + 'static`) and the `_with` pattern:

```rust
use enough::{CloneStop, Stop};

/// No-cancellation entry point
pub fn decode(data: &[u8]) -> Result<Vec<u8>, MyError> {
    decode_with(data, Unstoppable)
}

/// Full version: clonable stop for parallel paths
pub fn decode_with(data: &[u8], stop: impl CloneStop) -> Result<Vec<u8>, MyError> {
    let worker_stop = stop.clone();
    // spawn worker with worker_stop...
    inner(data, &stop)?; // &impl CloneStop → &dyn Stop
    Ok(vec![])
}

// Internal: single implementation, no monomorphization
fn inner(data: &[u8], stop: &dyn Stop) -> Result<(), MyError> {
    let stop = stop.may_stop().then_some(stop);
    // hot loop...
    Ok(())
}
```

### Type-Erased Cloning: `DynStop`

`&dyn Stop` can't be cloned. `impl CloneStop` is clonable but
monomorphized. For **erased + clonable** (e.g., dynamic thread pools
where you clone per work item), use
[`DynStop`](https://docs.rs/almost-enough/latest/almost_enough/struct.DynStop.html)
from `almost-enough`:

```rust
use almost_enough::DynStop;

pub fn decode_with(data: &[u8], stop: impl CloneStop) -> Result<Vec<u8>, MyError> {
    let stop = DynStop::new(stop); // erase once at the boundary
    orchestrate(data, &stop)       // everything below is one implementation
}

fn orchestrate(data: &[u8], stop: &DynStop) -> Result<(), MyError> {
    for chunk in data.chunks(65536) {
        let s = stop.clone(); // Arc increment, not allocation
        pool.spawn(move || worker(chunk, &s));
    }
    Ok(())
}
```

`DynStop` wraps `Arc<dyn Stop>` with nesting prevention, `active_stop()`
collapsing, and `Clone` without requiring `Clone` on the wrapped type.
Prefer it over manual `Arc<dyn Stop + Send + Sync>`.

> **Future direction:** `DynStop` and `CloneStop` may move from
> `almost-enough` into `enough` in a future release, so library authors
> can get erased + clonable stop tokens without the extra dependency.

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

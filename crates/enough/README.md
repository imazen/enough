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

Accept `impl Stop + 'static` in your public API. See
[Choosing a Signature](#choosing-a-function-signature) below.

```rust
use enough::{Stop, StopReason};

pub fn decode(data: &[u8], stop: impl Stop + 'static) -> Result<Vec<u8>, MyError> {
    for (i, chunk) in data.chunks(1024).enumerate() {
        if i % 16 == 0 {
            stop.check()?;
        }
        // process...
    }
    Ok(vec![])
}

// Callers:
// decode(&data, Unstoppable)?;   // no cancellation
// decode(&data, stopper)?;       // with cancellation

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

### Public API: `impl Stop + 'static`

One function per operation. Callers pass `Unstoppable` explicitly
for no cancellation:

```rust
use enough::{Stop, StopReason};

pub fn decode(data: &[u8], stop: impl Stop + 'static) -> Result<Vec<u8>, MyError> {
    // ...
    Ok(vec![])
}

// Callers:
// decode(&data, Unstoppable)?;   // no cancellation
// decode(&data, stopper)?;       // with cancellation
```

The `'static` bound is needed for `StopToken::new()` internally.
Use `impl Stop` (without `'static`) for embedded/no_std code that
accepts borrowed types like `StopRef<'a>`.

### Internally: use `StopToken` (from `almost-enough`)

[`StopToken`] is the best all-around choice for internal code. Benchmarks
show it within 3% of fully-inlined generic for `Unstoppable`, and **25%
faster** than generic for `Stopper` (due to the flattened Arc and
automatic `Option` optimization).

```rust
use enough::Stop;
use almost_enough::StopToken;

pub fn decode(data: &[u8], stop: impl Stop + 'static) -> Result<Vec<u8>, MyError> {
    let stop = StopToken::new(stop); // erase once (no Clone needed on T)
    decode_inner(data, &stop)       // single implementation below
}

fn decode_inner(data: &[u8], stop: &StopToken) -> Result<Vec<u8>, MyError> {
    for (i, chunk) in data.chunks(1024).enumerate() {
        if i % 16 == 0 {
            stop.check()?; // Unstoppable: automatic no-op. Stopper: one dispatch.
        }
    }
    Ok(vec![])
}
```

`StopToken` handles the `Unstoppable` optimization automatically — no
`may_stop()` call needed. For parallel work, clone the `StopToken`
(cheap Arc increment). `Stopper`/`SyncStopper` convert at zero cost
via `Into` (same Arc, no double-wrapping).

### Without `almost-enough`

Use `&dyn Stop` with `may_stop().then_some()`. The result is
`Option<&dyn Stop>` which implements `Stop` — `None.check()` returns
`Ok(())`, `Some.check()` delegates:

```rust
fn inner(data: &[u8], stop: &dyn Stop) -> Result<(), MyError> {
    let stop = stop.may_stop().then_some(stop); // Option<&dyn Stop>
    for (i, chunk) in data.chunks(1024).enumerate() {
        if i % 16 == 0 {
            stop.check()?; // None → Ok(()), Some → one dispatch
        }
    }
    Ok(())
}
```

[`StopToken`]: https://docs.rs/almost-enough/latest/almost_enough/struct.StopToken.html

> **Future direction:** `StopToken` may move from `almost-enough` into
> `enough` in a future release, so library authors can get erased +
> clonable stop tokens without the extra dependency.

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

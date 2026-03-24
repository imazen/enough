# enough

Minimal cooperative cancellation trait for Rust.

[![CI](https://img.shields.io/github/actions/workflow/status/imazen/enough/ci.yml?branch=main&style=for-the-badge&label=CI)](https://github.com/imazen/enough/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/enough.svg?style=for-the-badge)](https://crates.io/crates/enough)
[![docs.rs](https://img.shields.io/docsrs/enough?style=for-the-badge)](https://docs.rs/enough)
[![codecov](https://img.shields.io/codecov/c/github/imazen/enough?style=for-the-badge)](https://codecov.io/gh/imazen/enough)
[![License](https://img.shields.io/crates/l/enough.svg?style=for-the-badge)](LICENSE-MIT)
[![MSRV](https://img.shields.io/badge/MSRV-1.89-blue.svg?style=for-the-badge)](https://blog.rust-lang.org/2025/05/15/Rust-1.89.0.html)

A `no_std`, zero-dependency trait for cooperative cancellation.

## The Trait

```rust
pub trait Stop: Send + Sync {
    /// Check if the operation should stop.
    /// Returns Ok(()) to continue, Err(StopReason) to stop.
    fn check(&self) -> Result<(), StopReason>;

    /// Returns true if the operation should stop (provided).
    fn should_stop(&self) -> bool { self.check().is_err() }

    /// Returns true if this stop can ever fire (provided).
    /// Unstoppable returns false. Used by StopToken/BoxedStop to
    /// optimize away no-op stops at construction time.
    fn may_stop(&self) -> bool { true }
}
```

One required method. `Option<T: Stop>` implements `Stop`: `None` is
a no-op, `Some` delegates — enabling the `may_stop()` optimization
pattern (see below).

## Quick Start

Accept `impl Stop + 'static` in your public API. Use
[`StopToken`](https://docs.rs/almost-enough/latest/almost_enough/struct.StopToken.html)
from `almost-enough` internally — it handles the `Unstoppable` optimization
automatically and is the fastest option for real stop types:

```rust
use enough::Stop;
use almost_enough::StopToken;

pub fn decode(data: &[u8], stop: impl Stop + 'static) -> Result<Vec<u8>, MyError> {
    let stop = StopToken::new(stop); // Unstoppable → None (no alloc). Stopper → same Arc.
    for (i, chunk) in data.chunks(1024).enumerate() {
        if i % 16 == 0 {
            stop.check()?; // Unstoppable: no-op. Stopper: one dispatch.
        }
        // process...
    }
    Ok(vec![])
}

// Callers:
// decode(&data, Unstoppable)?;   // no cancellation — zero cost
// decode(&data, stopper)?;       // with cancellation
```

`StopToken` is `Clone` (Arc increment) for thread fan-out.
`Stopper`/`SyncStopper` convert to `StopToken` at zero cost via `Into`
(same Arc, no double-wrapping). Benchmarks show `StopToken` within 3%
of fully-inlined generic for `Unstoppable`, and 25% faster than generic
for `Stopper`.

### Without `almost-enough`

Use `&dyn Stop` with `may_stop().then_some()`:

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

### Embedded / no_std

Use `impl Stop` (without `'static`) to accept borrowed types like
`StopRef<'a>`:

```rust
fn process(data: &[u8], stop: impl Stop) -> Result<(), StopReason> {
    for (i, byte) in data.iter().enumerate() {
        if i % 64 == 0 { stop.check()?; }
    }
    Ok(())
}
```

## Crate Structure

| Crate | Purpose |
|-------|---------|
| [`enough`](https://crates.io/crates/enough) | Core trait: `Stop`, `StopReason`, `Unstoppable` |
| [`almost-enough`](https://crates.io/crates/almost-enough) | All implementations: `Stopper`, `StopToken`, `StopSource`, timeouts, combinators |
| [`enough-ffi`](https://crates.io/crates/enough-ffi) | C FFI for cross-language use |
| [`enough-tokio`](https://crates.io/crates/enough-tokio) | Bridge to tokio's CancellationToken |

## Features

- **None (default)** - `no_std` core: `Stop` trait, `StopReason`, `Unstoppable`
- **`alloc`** - Adds `Box<T>` and `Arc<T>` blanket impls for `Stop`
- **`std`** - Implies `alloc` (kept for downstream compatibility)

## License

MIT OR Apache-2.0

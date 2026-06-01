# enough

Minimal cooperative cancellation trait for Rust.

[![CI](https://img.shields.io/github/actions/workflow/status/imazen/enough/ci.yml?branch=main&style=flat-square&label=CI)](https://github.com/imazen/enough/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/enough.svg?style=flat-square)](https://crates.io/crates/enough)
[![docs.rs](https://img.shields.io/docsrs/enough?style=flat-square)](https://docs.rs/enough)
[![codecov](https://img.shields.io/codecov/c/github/imazen/enough?style=flat-square)](https://codecov.io/gh/imazen/enough)
[![License](https://img.shields.io/crates/l/enough.svg?style=flat-square)](LICENSE-MIT)
[![MSRV](https://img.shields.io/badge/MSRV-1.85-blue.svg?style=flat-square)](https://blog.rust-lang.org/2025/02/20/Rust-1.85.0.html)

A `no_std`, zero-dependency trait for cooperative cancellation. One required
method, one zero-cost no-op type. Long-running operations accept a `Stop` and
check it periodically; callers that don't need cancellation pass `Unstoppable`,
which optimizes away to nothing.

```toml
[dependencies]
enough = "0.4.4"
```

```rust
use enough::{Stop, StopReason, Unstoppable};

// A function that can be cancelled mid-flight.
fn sum_chunks(data: &[u8], stop: impl Stop) -> Result<u64, StopReason> {
    let mut total = 0u64;
    for (i, chunk) in data.chunks(1024).enumerate() {
        if i % 16 == 0 {
            stop.check()?; // Ok(()) → keep going, Err(StopReason) → bail out
        }
        total += chunk.iter().map(|&b| b as u64).sum::<u64>();
    }
    Ok(total)
}

// No cancellation needed — `Unstoppable::check()` inlines to nothing.
let data = [1u8; 4096];
assert_eq!(sum_chunks(&data, Unstoppable).unwrap(), 4096);
```

To actually cancel something, you need a concrete stop type — a `Stopper` you can
flip from another thread, a timeout, a tree of child cancellations. Those live in
[`almost-enough`](https://docs.rs/almost-enough); this crate is just the trait
plus `Unstoppable`, so library authors can accept cancellation without pulling in
allocation or any dependencies.

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

Can't add a dependency? See [`ZERO-DEP.md`](ZERO-DEP.md).

## Features

- **None (default)** - `no_std` core: `Stop` trait, `StopReason`, `Unstoppable`
- **`alloc`** - Adds `Box<T>` and `Arc<T>` blanket impls for `Stop`
- **`std`** - Implies `alloc` (kept for downstream compatibility)

## License

MIT OR Apache-2.0

# enough

A cancellation trait for long-running library code. A function that takes a
`Stop` calls `check()` as it works and returns early when the caller cancels
or a deadline passes. The caller decides what can stop it, including nothing:
`Unstoppable` makes every check compile away. `no_std`, no dependencies.

This crate holds only the trait and its result types, so a library can accept
cancellation without pulling in an implementation. The stops callers create
(`Stopper`, timeouts, parent-child trees) are in
[`almost-enough`](https://crates.io/crates/almost-enough).

```toml
[dependencies]
enough = "0.4.5"
```

## Accepting a stop

Check every so often in the loop, and return the `StopReason` inside your own
error type:

```rust
use enough::{Stop, StopReason, Unstoppable};

#[derive(Debug)]
pub enum DecodeError {
    Corrupt,
    Stopped(StopReason),
}

impl From<StopReason> for DecodeError {
    fn from(reason: StopReason) -> Self {
        DecodeError::Stopped(reason)
    }
}

pub fn decode(data: &[u8], stop: impl Stop) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::with_capacity(data.len());
    for row in data.chunks(1024) {
        stop.check()?;
        out.extend_from_slice(row);
    }
    Ok(out)
}

// A caller that doesn't need to cancel.
let pixels = decode(&[1, 2, 3], Unstoppable).unwrap();
assert_eq!(pixels, [1, 2, 3]);
```

For the common stops `check()` is a load and a branch, so checking once per row
or per block costs little.

## Behind `&dyn Stop`

Generic code (`impl Stop`) inlines `Unstoppable`'s checks to nothing. Behind
`&dyn Stop` each check is a call, so call `live()` once before a hot loop: it
returns `None` for a stop that can never fire, and `Option<&dyn Stop>`
implements `Stop`.

```rust
use enough::{Stop, StopReason, Unstoppable};

fn sum(values: &[u32], stop: &dyn Stop) -> Result<u64, StopReason> {
    let stop = stop.live();
    let mut total = 0;
    for &v in values {
        stop.check()?; // a branch, not a call, for Unstoppable
        total += u64::from(v);
    }
    Ok(total)
}

assert_eq!(sum(&[1, 2, 3], &Unstoppable), Ok(6));
```

## Telling cancellation from failure

`AsStopReason` lets a caller ask whether an error represents a cancellation or
timeout, for example to skip a retry, without knowing the error type. Implement
it next to your `From<StopReason>`:

```rust
use enough::{AsStopReason, StopReason};

enum DecodeError {
    Corrupt,
    Stopped(StopReason),
}

impl AsStopReason for DecodeError {
    fn as_stop_reason(&self) -> Option<StopReason> {
        match self {
            DecodeError::Stopped(reason) => Some(*reason),
            DecodeError::Corrupt => None,
        }
    }
}

assert_eq!(
    DecodeError::Stopped(StopReason::TimedOut).as_stop_reason(),
    Some(StopReason::TimedOut)
);
assert!(DecodeError::Corrupt.as_stop_reason().is_none());
```

## Implementing `Stop`

One method is required. A stop must be `Send + Sync`:

```rust
use core::sync::atomic::{AtomicBool, Ordering};
use enough::{Stop, StopReason};

struct Flag<'a>(&'a AtomicBool);

impl Stop for Flag<'_> {
    fn check(&self) -> Result<(), StopReason> {
        if self.0.load(Ordering::Relaxed) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }
}

let cancelled = AtomicBool::new(false);
let stop = Flag(&cancelled);
assert!(stop.check().is_ok());
cancelled.store(true, Ordering::Relaxed);
assert_eq!(stop.check(), Err(StopReason::Cancelled));
```

`Stop` is also implemented for `&T`, `&mut T` and `Option<T>`, and with the
`alloc` feature for `Box<T>` and `Arc<T>`.

## Features

- default: none. `no_std` and no allocator.
- `alloc`: `Stop` for `Box<T>` and `Arc<T>`.
- `std`: implies `alloc`.

The minimum supported Rust version is 1.85.

## Related crates

- [`almost-enough`](https://crates.io/crates/almost-enough): `Stopper`,
  `StopSource`, timeouts, `ChildStopper`, and `StopToken` for type erasure.
- [`enough-tokio`](https://crates.io/crates/enough-tokio): a `Stop` for
  tokio's `CancellationToken`.
- [`enough-ffi`](https://crates.io/crates/enough-ffi): cancellation across a C
  ABI.

## License

MIT OR Apache-2.0.

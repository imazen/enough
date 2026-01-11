# enough

Minimal cooperative cancellation for Rust.

[![Crates.io](https://img.shields.io/crates/v/enough.svg)](https://crates.io/crates/enough)
[![Documentation](https://docs.rs/enough/badge.svg)](https://docs.rs/enough)
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

## For Application Developers

```toml
[dependencies]
enough = { version = "0.1", features = ["std"] }
```

```rust
use enough::{Stopper, Stop, TimeoutExt};
use std::time::Duration;

let stop = Stopper::new();
let timed = stop.clone().with_timeout(Duration::from_secs(30));

// Pass to library
let result = my_lib::decode(&data, timed);

// Or cancel manually
stop.cancel();
```

## Zero-Cost Default

```rust
use enough::Never;

// Compiles away completely - zero runtime cost
let result = my_lib::decode(&data, Never);
```

## Features

- **None (default)** - `no_std` core: `Stop` trait, `Never`, `StopSource`, `FnStop`, `OrStop`
- **`alloc`** - Adds `Stopper`, `SyncStopper`, `ChildStopper`, `BoxedStop` + `Box<T>`/`Arc<T>` impls
- **`std`** - Implies `alloc`. Adds timeouts (`TimeoutExt`, `WithTimeout`)

## Type Overview

| Type | Feature | Use Case |
|------|---------|----------|
| `Never` | core | Zero-cost "never stop" |
| `StopSource` / `StopRef` | core | Stack-based, borrowed, Relaxed ordering |
| `FnStop` | core | Wrap any closure |
| `OrStop` | core | Combine multiple stop sources |
| `Stopper` | alloc | **Default choice** - Arc-based, clone to share |
| `SyncStopper` | alloc | Like Stopper with Acquire/Release ordering |
| `ChildStopper` | alloc | Hierarchical parent-child cancellation |
| `BoxedStop` | alloc | Type-erased dynamic dispatch |
| `WithTimeout` | std | Add deadline to any Stop |

## See Also

- [`almost-enough`](https://crates.io/crates/almost-enough) - Ergonomic extensions (`.or()`, `.into_boxed()`, `.child()`, guards)
- [`enough-ffi`](https://crates.io/crates/enough-ffi) - FFI helpers for C#, Python, Node.js
- [`enough-tokio`](https://crates.io/crates/enough-tokio) - Tokio CancellationToken bridge

**Note:** Ergonomic extensions live in `almost-enough` until stabilized from use and feedback.

## License

MIT OR Apache-2.0

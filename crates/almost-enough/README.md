# almost-enough

Batteries-included ergonomic extensions for the [`enough`](https://crates.io/crates/enough) cooperative cancellation crate.

While [`enough`](https://crates.io/crates/enough) provides only the minimal `Stop` trait, this crate provides all concrete implementations, combinators, and helpers. It re-exports everything from `enough` for convenience.

## Install

```toml
[dependencies]
almost-enough = "0.4.5"
```

The default `std` feature pulls in everything (Arc-based stoppers, timeouts, guards). For `no_std`, disable default features and opt into `alloc` for the Arc-based types: `almost-enough = { version = "0.4.5", default-features = false, features = ["alloc"] }`. See [Features](#features).

## Start Here: `Stopper`

**If you're not sure which to use, reach for [`Stopper`].** It's the default: clones share one flag, and `cancel()` on any of them stops them all. The others, one line each:

- [`ChildStopper`], from `stop.child()`, builds a tree: cancelling a parent stops its children, and cancelling a child leaves its parent and siblings running.
- [`OrStop`], from `a.or(b)`, stops when either one does, such as a cancel button or a shutdown signal.
- [`StopSource`] is a flag on the stack or in a `static`, with no allocation; it lends out [`StopRef`]s with `as_ref()` and works in `no_std`.
- `.with_timeout(Duration::from_secs(30))`, from [`TimeoutExt`], adds a deadline to any stop.
- [`StopToken`] is an owned stop to store or move into another thread; it clones with a reference-count bump.

The [type table below](#type-overview) lists the rest.

## Quick Start

```rust
use almost_enough::{Stopper, Stop};

let stop = Stopper::new();
let stop2 = stop.clone();  // Clone to share the same flag

// Check it (any clone)
assert!(!stop2.should_stop());

// Cancel it (any clone)
stop.cancel();
assert!(stop2.should_stop());
```

The two methods you need:

- **`stop.cancel()`** — flip the flag. `cancel(&self)` is an **inherent method on `Stopper`** (it takes `&self`, not `self`, so it never consumes the stopper), and it's idempotent. Construct with `Stopper::new()`.
- **`stop.should_stop() -> bool`** — `true` once cancelled. This comes from the **`Stop` trait** (`almost_enough::Stop`), which `Stopper` implements directly, so the `Stop` trait must be in scope to call it.

Prefer the `?` idiom in fallible code: **`stop.check()?`** returns `Result<(), StopReason>` — `Ok(())` while running, `Err(StopReason::Cancelled)` once cancelled. `check` is also a `Stop`-trait method, and `should_stop()` is just `self.check().is_err()`.

## Cancel a Worker Thread

`Stopper` is `Send + Sync` (it's an `Arc<AtomicBool>` under the hood, and the `Stop` trait requires `Send + Sync`), so a clone can move into another thread and the original can cancel it from across the thread boundary:

```rust
use almost_enough::{Stopper, Stop};
use std::thread;

// `Stopper` is Send + Sync, so a clone is safe to move into a worker thread.
fn assert_send_sync<T: Send + Sync>() {}
assert_send_sync::<Stopper>();

let stop = Stopper::new();
let worker_stop = stop.clone(); // clone shares the same cancellation flag

let handle = thread::spawn(move || {
    let mut iterations = 0u64;
    // Loop until the parent thread asks us to stop.
    while !worker_stop.should_stop() {
        // ... do a chunk of work ...
        iterations += 1;
        if iterations > 10_000_000 {
            break; // safety valve for the doctest
        }
    }
    iterations
});

// Cancel from the parent thread (or a signal handler, a timeout, etc.).
stop.cancel();

let done = handle.join().unwrap();
let _ = done;
```

Inside a fallible worker, use `check()?` instead of the `while` loop:

```rust
use almost_enough::{Stopper, Stop, StopReason};
use std::thread;

let stop = Stopper::new();
let worker_stop = stop.clone();

let handle = thread::spawn(move || -> Result<(), StopReason> {
    for _ in 0..10_000_000u64 {
        worker_stop.check()?; // returns Err(StopReason::Cancelled) once cancelled
        // ... do work ...
    }
    Ok(())
});

stop.cancel();
// The worker returns Err(StopReason::Cancelled) (or Ok if it finished first).
let _ = handle.join().unwrap();
```

### Accepting a stop in your own functions

Library code usually accepts an `Option<&dyn Stop>`; a caller passes `Some(&stop)`, or `None` when nothing should stop the work. To store the stop or move it into another thread, accept a [`StopToken`] instead (see [below](#keeping-a-stop-stoptoken)):

```rust
use almost_enough::{Stopper, Stop, StopReason};

fn run(stop: Option<&dyn Stop>) -> Result<(), StopReason> {
    for _ in 0..1000 {
        stop.check()?; // Err(StopReason::Cancelled) once cancelled
        // ... work ...
    }
    Ok(())
}

let stop = Stopper::new();
run(Some(&stop)).unwrap();
run(None).unwrap();
```

Check every 10 ms of work or so: often enough that cancelling feels immediate, rarely enough that the call through `&dyn` costs nothing measurable.

## Type Overview

| Type | Feature | Use Case |
|------|---------|----------|
| [`Unstoppable`] | core | Zero-cost "never stop" |
| [`StopSource`] / [`StopRef`] | core | Stack-based, borrowed, zero-alloc |
| [`FnStop`] | core | Wrap any closure |
| [`OrStop`] | core | Combine multiple stops |
| [`Stopper`] | alloc | **Default choice** - Arc-based, clone to share |
| [`SyncStopper`] | alloc | Like Stopper, but every check is an Acquire load |
| [`ChildStopper`] | alloc | Hierarchical parent-child cancellation |
| [`StopToken`] | alloc | **Type-erased dynamic dispatch** - Arc-based, `Clone` |
| [`BoxedStop`] | alloc | Deprecated: use `StopToken` |
| [`WithTimeout`] | std | Add deadline to any `Stop` (reads the clock every check) |
| [`DebouncedTimeout`] | std | Like `WithTimeout`, reads the clock every N ≤ 64 checks, N timed from two checks |

[`Unstoppable`]: https://docs.rs/almost-enough/latest/almost_enough/struct.Unstoppable.html
[`StopSource`]: https://docs.rs/almost-enough/latest/almost_enough/struct.StopSource.html
[`StopRef`]: https://docs.rs/almost-enough/latest/almost_enough/struct.StopRef.html
[`FnStop`]: https://docs.rs/almost-enough/latest/almost_enough/struct.FnStop.html
[`OrStop`]: https://docs.rs/almost-enough/latest/almost_enough/struct.OrStop.html
[`Stopper`]: https://docs.rs/almost-enough/latest/almost_enough/struct.Stopper.html
[`SyncStopper`]: https://docs.rs/almost-enough/latest/almost_enough/struct.SyncStopper.html
[`ChildStopper`]: https://docs.rs/almost-enough/latest/almost_enough/struct.ChildStopper.html
[`StopToken`]: https://docs.rs/almost-enough/latest/almost_enough/struct.StopToken.html
[`BoxedStop`]: https://docs.rs/almost-enough/latest/almost_enough/struct.BoxedStop.html
[`WithTimeout`]: https://docs.rs/almost-enough/latest/almost_enough/struct.WithTimeout.html
[`DebouncedTimeout`]: https://docs.rs/almost-enough/latest/almost_enough/struct.DebouncedTimeout.html
[`TimeoutExt`]: https://docs.rs/almost-enough/latest/almost_enough/trait.TimeoutExt.html

## Features

- **`std`** (default) - Full functionality including timeouts
- **`alloc`** - Arc-based types, `into_token()`, `child()`, guards
- **None** - Core trait and stack-based types only (`no_std` compatible)

## Extension Traits

The [`StopExt`](https://docs.rs/almost-enough/latest/almost_enough/trait.StopExt.html) trait adds combinator methods to any `Stop`:

```rust
use almost_enough::{StopSource, Stop, StopExt};

let timeout = StopSource::new();
let cancel = StopSource::new();

// Combine: stop if either stops
let combined = timeout.as_ref().or(cancel.as_ref());
assert!(!combined.should_stop());

cancel.cancel();
assert!(combined.should_stop());
```

## Hierarchical Cancellation

Create child stops that inherit cancellation from their parent:

```rust
use almost_enough::{Stopper, Stop, StopExt};

let parent = Stopper::new();
let child = parent.child();

// Child cancellation doesn't affect parent
child.cancel();
assert!(!parent.should_stop());

// But parent cancellation propagates to children
let child2 = parent.child();
parent.cancel();
assert!(child2.should_stop());
```

## Stop Guards (RAII)

Automatically cancel on scope exit unless explicitly disarmed:

```rust
use almost_enough::{Stopper, StopDropRoll};

fn do_work(source: &Stopper) -> Result<(), &'static str> {
    let guard = source.stop_on_drop();

    // If we return early or panic, source is stopped
    risky_operation()?;

    // Success! Don't stop.
    guard.disarm();
    Ok(())
}

fn risky_operation() -> Result<(), &'static str> {
    Ok(())
}
```

## Keeping a stop: `StopToken`

A `&dyn Stop` is borrowed for one call. To store a stop or move it into another
thread, take a [`StopToken`]: it's owned, and clones with a reference-count
bump. [`Stopper`] and [`SyncStopper`] convert to `StopToken` via `Into` without
allocating — the existing Arc is reused. A `Stopper` is then checked directly,
a `SyncStopper` through the vtable. `into_token()` converts any other
`Stop + 'static`:

```rust
use almost_enough::{Stop, StopExt, StopToken, Stopper, TimeoutExt};
use std::thread;
use std::time::Duration;

struct Worker {
    stop: StopToken,
}

impl Worker {
    fn spawn(&self) -> thread::JoinHandle<()> {
        let stop = self.stop.clone(); // cheap Arc increment, no allocation
        thread::spawn(move || {
            while !stop.should_stop() {
                thread::yield_now();
            }
        })
    }
}

let stopper = Stopper::new();
let worker = Worker { stop: stopper.clone().into() }; // reuses the Stopper's Arc
let handle = worker.spawn();
stopper.cancel();
handle.join().unwrap();

// Any other stop, such as one with a deadline:
let worker = Worker {
    stop: Stopper::new().with_timeout(Duration::from_secs(30)).into_token(),
};
```

## Optimizing Hot Loops with `dyn Stop`

Call `live()` once to skip overhead for no-op stops behind `&dyn Stop`:

```rust
use almost_enough::{Stop, StopReason, Unstoppable};

fn process(stop: &dyn Stop) -> Result<(), StopReason> {
    let stop = stop.live(); // Option<&dyn Stop>: None if it can never stop
    for i in 0..1_000_000 {
        stop.check()?; // None → Ok(()), Some → one vtable dispatch
    }
    Ok(())
}

// Unstoppable can never stop, so stop is None — zero overhead
assert!(process(&Unstoppable).is_ok());
```

`StopToken` automatically optimizes away no-op stops — when
wrapping `Unstoppable`, `check()` short-circuits without any vtable dispatch:

```rust
use almost_enough::{StopToken, Stopper, Unstoppable, Stop, StopReason};

fn hot_loop(stop: &StopToken) -> Result<(), StopReason> {
    for i in 0..1_000_000 {
        stop.check()?; // Unstoppable: no-op. Stopper: one dispatch.
    }
    Ok(())
}

hot_loop(&StopToken::new(Unstoppable)).unwrap();    // zero overhead
hot_loop(&StopToken::new(Stopper::new())).unwrap(); // one dispatch per check
```

## See Also

- [`enough`](https://crates.io/crates/enough) - Minimal core trait (for library authors)
- [`enough-tokio`](https://crates.io/crates/enough-tokio) - Tokio CancellationToken bridge
- [`enough-ffi`](https://crates.io/crates/enough-ffi) - FFI helpers for C#, Python, Node.js

## License

MIT OR Apache-2.0

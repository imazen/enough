# enough

Cooperative cancellation for Rust. Decoding an image, compressing a file or
running a search can take long enough that someone wants it stopped: the window
closed, a deadline passed, a newer job replaced it. A library checks a `Stop` as
it works; whoever calls it decides what can stop it.

## Stopping work

Make a `Stopper`, from [`almost-enough`](https://crates.io/crates/almost-enough),
hand it to the work, and cancel it from anywhere that holds a clone:

```rust
use almost_enough::Stopper;
use enough::Stop;

let stop = Stopper::new();
let worker = {
    let stop = stop.clone();
    std::thread::spawn(move || {
        // Any work that checks `stop`, such as `codec::decode(&bytes, Some(&stop))`.
        while !stop.should_stop() {
            std::thread::yield_now();
        }
    })
};
stop.cancel(); // from a cancel button, a deadline watcher, a shutdown hook
worker.join().unwrap();
```

almost-enough has a stop for most situations:

- `Stopper` is the default: clones share one flag, and `cancel()` on any of
  them stops them all.
- `ChildStopper`, from `stop.child()`, builds a tree: cancelling a parent stops
  its children, and cancelling a child leaves its parent and siblings running.
- `OrStop`, from `a.or(b)`, stops when either one does, such as a cancel button
  or a shutdown signal.
- `StopSource` is a flag on the stack or in a `static`, with no allocation; it
  lends out `StopRef`s with `as_ref()` and works in `no_std`.
- `.with_timeout(Duration::from_secs(30))`, from `TimeoutExt`, adds a deadline
  to any stop.
- `Unstoppable`, from enough itself, never stops.

## Making your library cooperative

Accept an `Option<&dyn Stop>`, check it as you work, and return early when it
says so:

```rust
use enough::{Stop, StopReason};

pub fn compress(input: &[u8], stop: Option<&dyn Stop>) -> Result<Vec<u8>, StopReason> {
    let mut output = Vec::new();
    for block in input.chunks(64 * 1024) {
        stop.check()?; // Err once the caller cancels or a deadline passes
        output.extend_from_slice(block); // the real work goes here
    }
    Ok(output)
}

// A caller that doesn't need to cancel passes `None`.
assert_eq!(compress(b"some data", None).unwrap(), b"some data");
```

Aim for a check every 10 ms of work or so: often enough that cancelling feels
immediate, rarely enough that the checks cost nothing measurable. Paced like
that, the call through `&dyn` doesn't matter, and it's usually better than
monomorphizing your code for every stop type. In a real library the `StopReason`
travels inside your own error type through `From<StopReason>`, and `?` keeps
working.

Strictly, the `Option` isn't needed: a `&dyn Stop` parameter takes
`&Unstoppable`, and `stop.live()` turns a stop that can never fire into `None`
for a hot loop. But `None` is what most callers reach for first.

A `&dyn Stop` is borrowed for the call: scoped threads and rayon can share it,
but it can't move into `thread::spawn` or a spawned task. If you need to store,
move or thread the stop, accept a
[`StopToken`](https://docs.rs/almost-enough/latest/almost_enough/struct.StopToken.html)
instead: it's owned, clones with a reference-count bump, and when made from a
`Stopper` checks it without a vtable call. `impl Stop + 'static` offers all of
it, monomorphized checks included, and converts to a `StopToken` when you need
one.

## Without the dependency

If you'd rather not put `enough`'s types in your public API, a closure is the
common currency: accept `impl Fn() -> bool`, and a caller holding a stop passes
`|| stop.should_stop()`.
[ZERO-DEP.md](https://github.com/imazen/enough/blob/main/ZERO-DEP.md) has two
pieces to copy instead: a 25-line trait that closures implement, and a one-file
handle that also keeps the `StopReason`. Both bridge to and from `enough` in one
line. zune-jpeg ships the trait this way, polling it once every 1024 MCUs:
[`cancel.rs`](https://github.com/etemesi254/zune-image/blob/52300d398488e907283786da5196e7f1797cfd81/crates/zune-jpeg/src/cancel.rs#L43).

## Why a separate crate

`enough` holds only the trait and two small types, with no dependencies and
`no_std` support. A library can accept cancellation without choosing a runtime
or a cancellation type for its users, and every library that uses it accepts the
same thing. The implementations live elsewhere:

- [`almost-enough`](https://crates.io/crates/almost-enough): `Stopper`,
  timeouts, parent-child cancellation, `StopToken`
- [`enough-tokio`](https://crates.io/crates/enough-tokio): tokio's
  `CancellationToken`
- [`enough-ffi`](https://crates.io/crates/enough-ffi): cancellation from C and
  other languages

The API documentation is on [docs.rs](https://docs.rs/enough).

MIT OR Apache-2.0.

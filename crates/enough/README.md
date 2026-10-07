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
        // Any work that checks `stop`, such as `codec::decode(&bytes, &stop)`.
        while !stop.should_stop() {
            std::thread::yield_now();
        }
    })
};
stop.cancel(); // from a cancel button, a deadline watcher, a shutdown hook
worker.join().unwrap();
```

For a deadline, almost-enough's `TimeoutExt` adds
`.with_timeout(Duration::from_secs(30))` to any stop. When nothing should stop
the work, pass `Unstoppable`.

## Making your library cooperative

Accept a `&dyn Stop`, check it once per row, block or iteration, and return
early when it says so:

```rust
use enough::{Stop, StopReason};

pub fn compress(input: &[u8], stop: &dyn Stop) -> Result<Vec<u8>, StopReason> {
    let mut output = Vec::new();
    for block in input.chunks(64 * 1024) {
        stop.check()?; // Err once the caller cancels or the deadline passes
        output.extend_from_slice(block); // the real work goes here
    }
    Ok(output)
}
```

In a real library the `StopReason` travels inside your own error type through
`From<StopReason>`, and `?` keeps working. `&dyn Stop` keeps your code free of
generics; accept `Option<&dyn Stop>` instead if callers should be able to pass
`None`. For a hot loop, `stop.live()` turns a stop that can never fire into
`None`, so the check becomes a branch instead of a call.

A `&dyn Stop` is borrowed for the call: scoped threads and rayon can share it,
but it can't move into `thread::spawn` or a spawned task. To keep a stop, or to
clone it across threads, accept an owned one: `impl Stop + 'static` (put it in
an `Arc` to share it), or a
[`StopToken`](https://docs.rs/almost-enough/latest/almost_enough/struct.StopToken.html),
which clones with a reference-count bump and checks a `Stopper` without a
vtable call.

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

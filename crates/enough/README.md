# enough

Cooperative cancellation for Rust libraries.

Decoding an image, compressing a file or running a search can take long enough
that someone wants it stopped: the window closed, the request timed out, a newer
job replaced it. `enough` gives libraries one small way to support that. A
function accepts a `&dyn Stop` and checks it as it works; the caller decides
what can stop it.

```rust
use enough::{Stop, StopReason, Unstoppable};

pub fn compress(input: &[u8], stop: &dyn Stop) -> Result<Vec<u8>, StopReason> {
    let mut output = Vec::new();
    for block in input.chunks(64 * 1024) {
        stop.check()?; // returns early once the caller cancels
        output.extend_from_slice(block); // the real work goes here
    }
    Ok(output)
}

// A caller that never cancels passes `Unstoppable`.
let output = compress(b"some data", &Unstoppable).unwrap();
assert_eq!(output, b"some data");
```

To cancel, the caller holds something it can trip. A `Stopper`, from
[`almost-enough`](https://crates.io/crates/almost-enough), is a flag shared by
its clones:

```rust
use almost_enough::Stopper;
use enough::Stop;

let stop = Stopper::new();
let cancel = stop.clone(); // hand this to a UI button or another thread
std::thread::spawn(move || cancel.cancel()).join().unwrap();
assert!(stop.should_stop());
```

Taking `&dyn Stop` keeps your function free of generics: one copy of the code,
whatever the caller passes. Check once per row, block or iteration; in a loop
hot enough for the call to matter, `stop.live()` turns a stop that can never
fire into `None`, and the check into a branch. In a real library the
`StopReason` travels inside your own error type through `From<StopReason>`, and
`?` keeps working.

## Why a separate crate

`enough` holds only the trait and two small types, with no dependencies and
`no_std` support. A library can accept cancellation without choosing a runtime
or a cancellation type for its users, and every library that uses it accepts the
same thing. Applications choose the implementation:

- [`almost-enough`](https://crates.io/crates/almost-enough): flags, timeouts,
  parent-child cancellation, type erasure
- [`enough-tokio`](https://crates.io/crates/enough-tokio): tokio's
  `CancellationToken`
- [`enough-ffi`](https://crates.io/crates/enough-ffi): cancellation from C and
  other languages

## Compatible without the dependency

A library that won't take even this dependency can still interoperate. A
closure is the common currency: take `impl Fn() -> bool`, and a caller holding
an `enough` stop passes `|| stop.should_stop()`.
[ZERO-DEP.md](https://github.com/imazen/enough/blob/main/ZERO-DEP.md) has two
pieces to copy instead: a 25-line trait that closures implement, and a one-file
handle that also keeps the `StopReason`. Both bridge to and from `enough` in one
line. zune-jpeg ships the trait this way, polling it once every 1024 MCUs:
[`cancel.rs`](https://github.com/etemesi254/zune-image/blob/52300d398488e907283786da5196e7f1797cfd81/crates/zune-jpeg/src/cancel.rs#L43).

The API documentation is on [docs.rs](https://docs.rs/enough).

MIT OR Apache-2.0.

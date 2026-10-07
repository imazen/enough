# enough

Cooperative cancellation for Rust libraries.

Decoding an image, compressing a file or running a search can take long enough
that someone wants it stopped: the window closed, the request timed out, a newer
job replaced it. `enough` gives libraries one small way to support that. A
function accepts a `Stop` and checks it as it works; the caller decides what can
stop it.

```rust
use enough::{Stop, StopReason, Unstoppable};

pub fn compress(input: &[u8], stop: impl Stop) -> Result<Vec<u8>, StopReason> {
    let mut output = Vec::new();
    for block in input.chunks(64 * 1024) {
        stop.check()?; // returns early once the caller cancels
        output.extend_from_slice(block); // the real work goes here
    }
    Ok(output)
}

// A caller that never cancels passes `Unstoppable`; its checks compile away.
let output = compress(b"some data", Unstoppable).unwrap();
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

For the common stops a check is a load and a branch, so checking once per row,
block or iteration costs little. In a real library the `StopReason` usually
travels inside your own error type through `From<StopReason>`, and `?` keeps
working.

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

The API documentation is on [docs.rs](https://docs.rs/enough).

MIT OR Apache-2.0.

# enough [![CI](https://img.shields.io/github/actions/workflow/status/imazen/enough/ci.yml?style=flat-square&label=CI)](https://github.com/imazen/enough/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/enough?style=flat-square)](https://crates.io/crates/enough) [![lib.rs](https://img.shields.io/crates/v/enough?style=flat-square&label=lib.rs&color=blue)](https://lib.rs/crates/enough) [![docs.rs](https://img.shields.io/docsrs/enough?style=flat-square)](https://docs.rs/enough) [![MSRV](https://img.shields.io/badge/MSRV-1.85-blue?style=flat-square)](https://doc.rust-lang.org/cargo/reference/manifest.html#the-rust-version-field) [![license](https://img.shields.io/crates/l/enough?style=flat-square)](#license) [![codecov](https://img.shields.io/codecov/c/github/imazen/enough?style=flat-square)](https://codecov.io/gh/imazen/enough)

A `no_std`, zero-dependency trait for cooperative cancellation. One required
method, one zero-cost no-op type. Long-running operations accept a `Stop` and
check it periodically; callers that don't need cancellation pass `None`.

## Quick start

```toml
[dependencies]
enough = "0.4.5"
```

```rust
use enough::{Stop, StopReason};

// A function that can be cancelled mid-flight.
fn sum_chunks(data: &[u8], stop: Option<&dyn Stop>) -> Result<u64, StopReason> {
    let mut total = 0u64;
    for (i, chunk) in data.chunks(1024).enumerate() {
        if i % 16 == 0 {
            stop.check()?; // Ok(()) → keep going, Err(StopReason) → bail out
        }
        total += chunk.iter().map(|&b| b as u64).sum::<u64>();
    }
    Ok(total)
}

// A caller that doesn't need to cancel passes `None`.
let data = [1u8; 4096];
assert_eq!(sum_chunks(&data, None).unwrap(), 4096);
```

`enough` gives a library the *consumer* side: accept a `Stop` and call
`check()`. It deliberately ships **no constructible token** — no allocation, no dependencies. To *produce* and *flip* a real
cancellation flag, an application reaches for `Stopper` in the sibling
[`almost-enough`](https://crates.io/crates/almost-enough) crate.

## Actually Cancel Something

`Stopper` is an `Arc`-backed flag: clone it to share one flag across threads, and
call `.cancel()` on any clone to flip them all. It implements `Stop`, so the same
function above accepts `Some(&stop)`.

```toml
[dependencies]
enough = "0.4.5"
almost-enough = "0.4.5"  # the constructible Stopper lives here
```

```rust,no_run
use std::thread;
use std::time::Duration;
use almost_enough::Stopper;
use enough::Stop; // Stopper implements it; brings `check()` into scope

let stop = Stopper::new();
let worker_stop = stop.clone(); // same flag, shared across threads

let worker = thread::spawn(move || {
    let mut ticks = 0u64;
    // Loop until another thread flips the flag.
    while worker_stop.check().is_ok() {
        ticks += 1;
        thread::sleep(Duration::from_millis(1));
    }
    ticks // returns once cancelled
});

// ...let it run, then cancel from this side.
thread::sleep(Duration::from_millis(20));
stop.cancel(); // flips every clone; check() now returns Err(StopReason::Cancelled)

let ticks = worker.join().unwrap();
assert!(ticks > 0); // it ran, then stopped when we cancelled
```

`stop.cancel()` is the flip method (idempotent), and `Stopper::cancelled()`
constructs one that is already tripped. `almost-enough` has a stop for most
situations:

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
- `StopToken` is an owned stop to store or move into another thread; it clones
  with a reference-count bump.

The rest is in its [docs](https://docs.rs/almost-enough).

## The Trait

```rust
use enough::StopReason;

pub trait Stop: Send + Sync {
    /// Check if the operation should stop.
    /// Returns Ok(()) to continue, Err(StopReason) to stop.
    fn check(&self) -> Result<(), StopReason>;

    /// Returns true if the operation should stop (provided).
    fn should_stop(&self) -> bool { self.check().is_err() }

    /// Returns true if this stop can ever fire (provided).
    /// Unstoppable returns false. Used by StopToken to
    /// optimize away no-op stops at construction time.
    fn may_stop(&self) -> bool { true }
}
```

One required method. `Option<T: Stop>` implements `Stop`: `None` is
a no-op, `Some` delegates — enabling the `may_stop()` optimization
pattern (see below).

## Integrating into a library

Accept an `Option<&dyn Stop>`, check it as you work, and return early when it
says so:

```rust
use enough::{Stop, StopReason};

pub fn decode(data: &[u8], stop: Option<&dyn Stop>) -> Result<Vec<u8>, StopReason> {
    for (i, chunk) in data.chunks(1024).enumerate() {
        if i % 16 == 0 {
            stop.check()?; // None: Ok(()). Some: one call through the vtable.
        }
        // process...
    }
    Ok(vec![])
}

// Callers:
// decode(&data, None)?;          // no cancellation
// decode(&data, Some(&stopper))?; // with cancellation
```

Aim for a check every 10 ms of work or so: often enough that cancelling feels
immediate, rarely enough that the checks cost nothing measurable. Paced like
that, the call through `&dyn` doesn't matter, and it's usually better than
monomorphizing your code for every stop type. In a real library the
`StopReason` travels inside your own error type through `From<StopReason>`.

A `&dyn Stop` is borrowed for the call: scoped threads and rayon can share it,
but it can't move into `thread::spawn` or a spawned task. To store, move or
thread the stop, accept a
[`StopToken`](https://docs.rs/almost-enough/latest/almost_enough/struct.StopToken.html)
from `almost-enough` instead:

```rust
use almost_enough::{Stopper, StopToken};
use enough::{Stop, StopReason};
use std::thread;

pub fn decode_in_background(
    data: Vec<u8>,
    stop: StopToken,
) -> thread::JoinHandle<Result<usize, StopReason>> {
    thread::spawn(move || {
        for chunk in data.chunks(1024) {
            stop.check()?;
            // process...
        }
        Ok(data.len())
    })
}

let stopper = Stopper::new();
let worker = decode_in_background(vec![0; 4096], stopper.clone().into());
assert_eq!(worker.join().unwrap(), Ok(4096));
```

`StopToken` is `Clone` (Arc increment) for thread fan-out. `Stopper` and
`SyncStopper` convert to it with `Into` without allocating (same Arc); a
`Stopper` is then checked directly, a `SyncStopper` through the vtable.
`StopToken::new` takes any `Stop + 'static`, and from `Unstoppable` it makes a
token whose checks do nothing. Accepting `impl Stop + 'static` offers all of
it, monomorphized checks included.

### Hot loops

Strictly, the `Option` isn't needed: a `&dyn Stop` parameter takes
`&Unstoppable`. Call `live()` once, before the loop, to turn a stop that can
never fire into `None`:

```rust
use enough::{Stop, StopReason};

fn inner(data: &[u8], stop: &dyn Stop) -> Result<(), StopReason> {
    let stop = stop.live(); // Option<&dyn Stop>: None if it can never stop
    for (i, chunk) in data.chunks(1024).enumerate() {
        if i % 16 == 0 {
            stop.check()?; // None → Ok(()), Some → one dispatch
        }
    }
    Ok(())
}
```

`Option<&dyn Stop>` and `&dyn Stop` need no allocator, so they work in
`no_std`, where `almost-enough`'s `StopSource` provides the flag.

## Crate Structure

| Crate | Purpose |
|-------|---------|
| [`enough`](https://crates.io/crates/enough) | Core trait: `Stop`, `StopReason`, `Unstoppable` |
| [`almost-enough`](https://crates.io/crates/almost-enough) | All implementations: `Stopper`, `StopToken`, `StopSource`, timeouts, combinators |
| [`enough-ffi`](https://crates.io/crates/enough-ffi) | C FFI for cross-language use |
| [`enough-tokio`](https://crates.io/crates/enough-tokio) | Bridge to tokio's CancellationToken |

Can't add a dependency? See [`ZERO-DEP.md`](https://github.com/imazen/enough/blob/main/ZERO-DEP.md).

## Features

- **None (default)** - `no_std` core: `Stop` trait, `StopReason`, `Unstoppable`
- **`alloc`** - Adds `Box<T>` and `Arc<T>` blanket impls for `Stop`
- **`std`** - Implies `alloc` (kept for downstream compatibility)

## License

Licensed under either of [MIT](https://github.com/imazen/enough/blob/main/LICENSE-MIT)
or [Apache-2.0](https://github.com/imazen/enough/blob/main/LICENSE-APACHE), at your option.

## Image tech I maintain

| | |
|:--|:--|
| **Codecs** ¹ | [zenjpeg] · [zenpng] · [zenwebp] · [zengif] · [zenavif] · [zenjxl] · [zenjxl-decoder] · [jxl-encoder] · [zenbitmaps] · [heic] · [zentiff] · [zenpdf] · [zensvg] · [zenjp2] · [zenraw] · [ultrahdr] |
| Codec internals | [zenrav1e] · [rav1d-safe] · [zenravif] · [zenavif-parse] · [zenavif-serialize] |
| Compression | [zenflate] · [zenzop] · [zenzstd] |
| Processing | [zenresize] · [zenquant] · [zenblend] · [zenfilters] · [zensally] · [zentone] |
| Pixels & color | [zenpixels] · [zenpixels-convert] · [linear-srgb] · [garb] · [zenyuv] |
| Pipeline & framework | [zenpipe] · [zencodec] · [zencodecs] · [zenlayout] · [zennode] · [zenwasm] · [zentract] |
| Metrics | [zensim] · [fast-ssim2] · [butteraugli] · [zenmetrics] · [resamplescope-rs] |
| Pickers & ML | [zenanalyze] · [zenpredict] · [zenpicker] · [zenanalyze-api] |
| Test corpora | [codec-corpus] · [imazen-26] |
| Products | [Imageflow] image engine ([.NET][imageflow-dotnet] · [Node][imageflow-node] · [Go][imageflow-go]) · [Imageflow Server] · [ImageResizer] (C#) |

<sub>¹ pure-Rust, `#![forbid(unsafe_code)]` codecs, as of 2026</sub>

### General Rust awesomeness

[zenbench] · [archmage] · [magetypes] · **enough** · [whereat] · [cargo-copter] · [zenutils]

[Open source](https://www.imazen.io/open-source) · [@imazen](https://github.com/imazen) · [@lilith](https://github.com/lilith) · [lib.rs/~lilith](https://lib.rs/~lilith)

[zenjpeg]: https://github.com/imazen/zenjpeg
[zenpng]: https://github.com/imazen/zenpng
[zenwebp]: https://github.com/imazen/zenwebp
[zengif]: https://github.com/imazen/zengif
[zenavif]: https://github.com/imazen/zenavif
[zenjxl]: https://github.com/imazen/zenjxl
[zenjxl-decoder]: https://github.com/imazen/zenjxl-decoder
[jxl-encoder]: https://github.com/imazen/jxl-encoder
[zenbitmaps]: https://github.com/imazen/zenbitmaps
[heic]: https://github.com/imazen/heic
[zentiff]: https://github.com/imazen/zenextras
[zenpdf]: https://github.com/imazen/zenextras
[zensvg]: https://github.com/imazen/zenextras
[zenjp2]: https://github.com/imazen/zenextras
[zenraw]: https://github.com/imazen/zenraw
[ultrahdr]: https://github.com/imazen/ultrahdr
[zenrav1e]: https://github.com/imazen/zenrav1e
[rav1d-safe]: https://github.com/imazen/rav1d-safe
[zenravif]: https://github.com/imazen/cavif-rs
[zenavif-parse]: https://github.com/imazen/zenavif
[zenavif-serialize]: https://github.com/imazen/zenavif
[zenflate]: https://github.com/imazen/zenflate
[zenzop]: https://github.com/imazen/zenzop
[zenzstd]: https://github.com/imazen/zenzstd
[zenresize]: https://github.com/imazen/zenresize
[zenquant]: https://github.com/imazen/zenquant
[zenblend]: https://github.com/imazen/zenblend
[zenfilters]: https://github.com/imazen/zenpipe
[zensally]: https://github.com/imazen/zensally
[zentone]: https://github.com/imazen/zentone
[zenpixels]: https://github.com/imazen/zenpixels
[zenpixels-convert]: https://github.com/imazen/zenpixels
[linear-srgb]: https://github.com/imazen/linear-srgb
[garb]: https://github.com/imazen/garb
[zenyuv]: https://github.com/imazen/zenjpeg
[zenpipe]: https://github.com/imazen/zenpipe
[zencodec]: https://github.com/imazen/zencodec
[zencodecs]: https://github.com/imazen/zenpipe
[zenlayout]: https://github.com/imazen/zenpipe
[zennode]: https://github.com/imazen/zennode
[zenwasm]: https://github.com/imazen/zenwasm
[zentract]: https://github.com/imazen/zentract
[zensim]: https://github.com/imazen/zensim
[fast-ssim2]: https://github.com/imazen/fast-ssim2
[butteraugli]: https://github.com/imazen/butteraugli
[zenmetrics]: https://github.com/imazen/zenmetrics
[resamplescope-rs]: https://github.com/imazen/resamplescope-rs
[zenanalyze]: https://github.com/imazen/zenanalyze
[zenpredict]: https://github.com/imazen/zenanalyze
[zenpicker]: https://github.com/imazen/zenanalyze
[zenanalyze-api]: https://github.com/imazen/zenanalyze
[codec-corpus]: https://github.com/imazen/codec-corpus
[imazen-26]: https://github.com/imazen/imazen-26
[zenbench]: https://github.com/imazen/zenbench
[archmage]: https://github.com/imazen/archmage
[magetypes]: https://github.com/imazen/archmage
[whereat]: https://github.com/lilith/whereat
[cargo-copter]: https://github.com/imazen/cargo-copter
[zenutils]: https://github.com/imazen/zenutils
[Imageflow]: https://github.com/imazen/imageflow
[Imageflow Server]: https://github.com/imazen/imageflow-dotnet-server
[ImageResizer]: https://github.com/imazen/resizer
[imageflow-dotnet]: https://github.com/imazen/imageflow-dotnet
[imageflow-node]: https://github.com/imazen/imageflow-node
[imageflow-go]: https://github.com/imazen/imageflow-go

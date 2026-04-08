# Zero-Dep Cancellation Pattern Compatible With `enough`

Copy [`tests/test-or-do-this/src/zerodep.rs`](tests/test-or-do-this/src/zerodep.rs)
into your crate. One file, zero deps, compatible with `enough` in
both directions.

## The shape

```rust
pub struct StopCheck {
    inner: Option<Arc<dyn Fn() -> Result<(), StopReason> + Send + Sync>>,
}
```

- `None` is the no-cancel case — zero cost, zero allocations, const.
- `Some(Arc<dyn Fn>)` captures the caller's cancellation state in a
  closure. One `Arc` allocation at construction; clone-cheap forever
  (refcount bump).
- 16 bytes, niche-optimized. Same memory layout as
  `enough::StopToken`.
- `StopReason` matches `enough::StopReason` variant-for-variant
  (`Cancelled`, `TimedOut`), implements `core::error::Error`.

The full file — including docs — is
[`tests/test-or-do-this/src/zerodep.rs`](tests/test-or-do-this/src/zerodep.rs).
Under 300 lines, most of which are docs and doctests. **You copy it
into your crate and stop thinking about it.**

## Why these exact choices

Each design decision is deliberate. If you deviate, you pay for it:

- **Owned, not borrowed (`StopCheck`, not `StopCheck<'a>`).** Lets
  you store it in structs and fan it out across threads without
  lifetime parameters propagating through your entire API.
- **`'static` closure.** Required for erased storage behind `Arc<dyn Fn>`
  and for crossing `thread::spawn`. The cost is that your closure
  can't borrow stack state — but because every real source
  (`Stopper`, `Arc<AtomicBool>`, `CancellationToken`) is already a
  clone-cheap handle, you just `move` a clone into the closure.
- **`Arc`, not `Box`.** `Box<dyn Fn>` isn't `Clone`, so you couldn't
  fan out a `StopCheck` to multiple threads or stash a copy in a
  child decoder. `Arc` gets you `Clone` for a single atomic
  increment.
- **`Result<(), StopReason>` closure return, not `bool`.** Matches
  `enough::Stop::check()` exactly, so lossless bridging is
  `move || token.check().map_err(...)`. A convenience constructor
  (`from_flag`) covers the common "just a bool" case.
- **`Send + Sync` on the closure.** Required to make `StopCheck`
  itself `Send + Sync` — which you need for thread fan-out, rayon
  parallel iterators, and async tasks.

## Storing it in your types

```rust
use your_crate::zerodep::{StopCheck, StopReason};

pub struct Decoder {
    stop: StopCheck,   // no lifetime parameter, no generics
    block_size: usize,
}

#[derive(Debug)]
pub enum DecodeError {
    Stopped(StopReason),
    InvalidData,
}

impl From<StopReason> for DecodeError {
    fn from(r: StopReason) -> Self { DecodeError::Stopped(r) }
}

impl Decoder {
    pub fn new(stop: StopCheck) -> Self {
        Self { stop, block_size: 1024 }
    }

    pub fn decode(&self, data: &[u8]) -> Result<Vec<u8>, DecodeError> {
        let mut out = Vec::with_capacity(data.len());
        for (i, chunk) in data.chunks(self.block_size).enumerate() {
            if i % 16 == 0 { self.stop.check()?; }
            out.extend_from_slice(chunk);
        }
        Ok(out)
    }
}
```

## Bridging from every source

One-line bridges from everything:

| Source | Bridge |
|--------|--------|
| Nothing | `StopCheck::none()` |
| `Arc<AtomicBool>` | `StopCheck::from_atomic(flag.clone())` |
| `enough` (cancel only) | `StopCheck::maybe_flag(stop.may_stop().then(\|\| ...))` |
| `enough` (reason-preserving) | `StopCheck::maybe(stop.may_stop().then(\|\| ...))` |
| `tokio_util::CancellationToken` | `StopCheck::from_flag(move \|\| token.is_cancelled())` |
| `crossbeam_channel::Receiver<()>` | `StopCheck::from_flag(move \|\| rx.try_recv().is_ok())` |

The reason-preserving bridge from `enough` is one `map_reason`
function you write once per crate, then every bridge is a one-liner:

```rust
use enough::StopReason as EReason;
use your_crate::zerodep::StopReason as ZReason;

/// Write once. enough's StopReason is #[non_exhaustive] so
/// the wildcard arm is required.
fn map_reason(r: EReason) -> ZReason {
    match r {
        EReason::Cancelled => ZReason::Cancelled,
        EReason::TimedOut  => ZReason::TimedOut,
        _                  => ZReason::Cancelled, // safe default
    }
}

// Then every bridge is this — `maybe` + `then` skips the Arc
// and the clone when may_stop() is false (e.g. Unstoppable):
StopCheck::maybe(token.may_stop().then(|| {
    let t = token.clone();
    move || t.check().map_err(map_reason)
}))
```

## Going the other way: your library calls an `enough`-using crate

Return a `StopToken` — it preserves the `may_stop()` optimization
in both directions. When `StopCheck` is `none()`, the `StopToken`
stores `None` internally (zero cost, same as `Unstoppable`):

```rust
use almost_enough::{FnStop, StopToken, Unstoppable};

fn to_enough_stop(stop: &StopCheck) -> StopToken {
    if stop.may_stop() {
        let s = stop.clone();
        StopToken::new(FnStop::new(move || s.check().is_err()))
    } else {
        StopToken::new(Unstoppable)
    }
}
```

`StopToken` is `Clone`, so you can fan the result out to rayon,
async tasks, or anywhere else. Both adapter directions preserve
`Clone` and `may_stop()` all the way down to the original flag.
See the `forward_adapter` and `reverse_adapter` test modules for
exhaustive validation including cross-thread fan-out and
`none()` round-trip.

## Naming and collisions

**`StopCheck`** is deliberately *not* called `StopToken` — when both
are in scope you can tell at a glance which is the closure-backed
handle and which is the trait-backed one.

**`StopReason`** deliberately *matches* `enough::StopReason` — same
variant names, so `impl From<StopReason> for MyError` is identical
for both. When bridging code needs both in scope, alias them:

```rust
use almost_enough::StopReason as EReason;
use your_crate::zerodep::StopReason as ZReason;
```

## Layout and cost

`StopCheck` is 16 bytes:

| Operation | Cost |
|-----------|------|
| `StopCheck::none()` | 0 bytes, `const`, zero allocations |
| `StopCheck::new(f)` / `from_flag(f)` | One `Arc` heap allocation |
| `.check()` (none) | One perfectly-predicted branch |
| `.check()` (some) | One branch + one indirect vtable call |
| `.clone()` | Atomic refcount increment; free for `none()` |

## When to upgrade to `enough`

Both `StopCheck` and `enough` support: reason distinction
(`Cancelled` / `TimedOut`), `core::error::Error`, clone-cheap
handles, `'static` storage, closure bridging, and `no_std + alloc`.

What `enough` adds:

| Feature | Crate |
|---------|-------|
| Zero-cost `Unstoppable` via generics | `enough` |
| Hierarchical (parent/child) cancellation | `almost-enough` |
| Built-in timeout composition | `almost-enough` |
| Drop guards (cancel-on-drop RAII) | `almost-enough` |
| `Or`-combinators (stop-if-either) | `almost-enough` |
| Ready-made bridges (tokio, FFI) | `enough-tokio`, `enough-ffi` |

`StopCheck` is the 80% you can ship in a single file. If you need
the other 20%, you need a crate — and `enough` is specifically that
crate.

## Going both ways

You don't have to commit. A library that ships `StopCheck` today can:

- **Add `enough` as a dep later** without breaking its users.
- **Be called by an `enough` user today** via the forward adapter.
- **Call an `enough`-using library today** via `FnStop`.
- **Be copy-pasted into yet another crate** — the file is
  self-contained and liberal-licensed.

## Validation

32 unit tests + 6 doctests cover standalone use, struct storage,
bidirectional `enough` interop, `Clone` preservation through the
full adapter chain, and reason-preserving bridges with actual
timeout firing. Run them with:

```bash
cargo test -p test-or-do-this
```

## Summary

1. Copy [`tests/test-or-do-this/src/zerodep.rs`](tests/test-or-do-this/src/zerodep.rs)
   into your crate.
2. Store `StopCheck` in your types. No lifetime parameters.
3. Call `self.stop.check()?` in your hot loops.
4. `impl From<StopReason> for YourError`.
5. Document that your users can bridge from anything.

Your crate is now cancellation-friendly with zero external deps
and a clear migration path into `enough` if and when you need more.

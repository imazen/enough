# Changelog

## [Unreleased]

### QUEUED BREAKING CHANGES

<!-- Breaks that ship together in the next leading-digit bump (0.5 for enough
and almost-enough). None queued. -->

## enough

### [0.4.5] - 2026-10-07

#### Added

- `AsStopReason`: the `StopReason` an error represents, if any; `StopReason` implements it (7e3fd28)
- `live()` on `dyn Stop` and its `+ Send` / `+ Send + Sync` forms: the stop, or `None` if it can never stop, for checking a `&dyn Stop` in a hot loop (ae5d376)

#### Changed

- `Stop::check`, `should_stop` and the forwarding impls are `#[track_caller]`, so an instrumented stop can attribute a poll to its call site; no signature change (5caac7e)
- The crates.io page is this crate's own README, whose examples compile as doctests, instead of the workspace README (bd674e8, 5b8315d, 3909cea)

## almost-enough

### [0.4.5] - 2026-10-07

#### Added

- `PollMeter<S>` behind the opt-in `poll-meter` feature: per-call-site histograms of the gaps between polls, flagging slow gaps and poll storms; `StopExt::metered()` wraps any stop (2236d17, 5caac7e)
- `DebouncedTimeout::clear_calibration()`: the next two checks read the clock and set how often it is read; calibration, at creation too, now times two consecutive checks (af0eaef)

#### Changed

- `cancel()` is a Release swap and `is_cancelled()` an Acquire load on `Stopper`, `StopSource`, `ChildStopper` and `SyncStopper`, so a thread whose `is_cancelled()` returns true sees what was written before the cancel it observed; checks are unchanged (4fd6dbd)
- `ChildStopper` walks `ChildStopper` parents without a vtable call per level; each node is 8 bytes larger (5283eda)
- `StopToken` drops its `SyncStopper` arm, so `check` is two branches instead of a jump table on x86-64 (5d53d06)
- `BoxedStop` wraps a `StopToken` and takes its fast paths; still not `Clone` (d2e41e8)
- `StopToken::from_arc(Arc<Stopper>)` checks the flag directly, as `StopToken::new` does; an `Arc<SyncStopper>` stays behind the vtable (c5d4d6c)
- `PollMeter` looks call sites up by `Location` address: 693 → 308 instructions per poll (a37d1cd)
- README: library functions take `Option<&dyn Stop>`, or a `StopToken` to store or thread the stop, instead of `impl Stop`; one line on each type to reach for (70302bf)
- Requires `enough` 0.4.5 (5b8315d)

#### Deprecated

- `BoxedStop` and `StopExt::into_boxed`, in favor of `StopToken` and `into_token()` (97d04f9)

#### Fixed

- `alloc` turns on `enough`'s `alloc`, so `Box<dyn Stop>` and `Arc<dyn Stop>` are stops in a build that uses only almost-enough (667b66b, 22cb41a)
- `DebouncedTimeout` reads the clock at least every 64 checks, so a slowdown can no longer make it stop seconds to a minute late; once one thread sharing it times out, every later check stops (e152409)
- Docs: `WithTimeout` states its per-check clock read, `DebouncedTimeout`'s calibration is described, and every README example compiles (0ca1e56, 69eb567)

## enough-tokio

### [0.5.1] - 2026-10-07

#### Changed

- A `TokioStop` checked more than 32 times registers a waker and from then on checks an atomic flag instead of locking the token: 47 → 15 instructions per check through `&dyn Stop`; `size_of::<TokioStop>()` is 40 bytes, was 8 (8a8ae32)
- Requires `tokio-util` 0.7.19 and `enough` 0.4.5 (d67e465, 5b8315d)
- README: the library example takes `Option<&dyn Stop>` instead of `impl Stop` (70302bf)

#### Fixed

- README: the dependencies a consumer needs, with versions that exist (c2dfcf3, 5b8315d)

## enough-ffi

### [0.4.1] - 2026-10-07

#### Changed

- `enough_cancellation_cancel` is a Release swap and `enough_cancellation_is_cancelled` an Acquire load; `enough_token_is_cancelled` stays a Relaxed load (4fd6dbd)
- Requires `enough` 0.4.5 (5b8315d)

#### Fixed

- README: `FfiCancellationToken::from_ptr`'s real signature and contracts, a C example, and which call is the Acquire query (c2dfcf3, 4fd6dbd)

## Workspace

### 2026-10-07 (with the releases above)

- `dev/cancel-latency`: harness that drives every zen codec through `PollMeter` (05735da, 9e1231a, 6bd6b96)
- Measurement records under `benchmarks/` for the changes above (8a8ae32, e518d2f)
- Versioned public-API snapshots at `docs/public-api/<crate>.txt` (b06dcb3, 6587550)
- CI runs the `poll-meter` tests and the cancel-handoff tests under Miri over 8 seeds (f6902bc, 4fd6dbd)
- README: rewritten as the GitHub landing page, without the criterion-era performance claims (fdb4c6a, cd0bc68, bd674e8, e6900f3); libraries take `Option<&dyn Stop>` or a `StopToken` (70302bf)

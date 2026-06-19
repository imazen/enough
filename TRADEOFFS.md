# Design Tradeoffs: `enough` / `almost-enough`

Captures design decisions, what we learned from benchmarking, and
the reasoning behind the current architecture.

## Core Architecture

```
enough (core, no_std, zero deps)
├── Stop trait: Send + Sync, check() + should_stop() + may_stop()
├── StopReason: Cancelled | TimedOut (1 byte)
├── Unstoppable: zero-cost no-op
├── impl Stop for Option<T: Stop>: None = no-op, Some = delegates
└── Blanket impls: &T, &mut T, Box<T>, Arc<T>

almost-enough (batteries, re-exports enough)
├── StopToken: Arc-based, Clone, automatic Unstoppable optimization
├── Stopper / SyncStopper: Arc<StopperInner>, zero-cost From<> → StopToken
├── StopSource / StopRef: stack-based, zero-alloc, borrowed
├── ChildStopper: hierarchical parent-child cancellation
├── BoxedStop: legacy, prefer StopToken
├── FnStop, OrStop, WithTimeout, CancelGuard
├── StopExt: .or(), .into_token(), .into_boxed(), .child()
└── ClonableStop: trait alias for Stop + Clone + 'static
```

## Key Decisions

### 1. `impl Stop + 'static` for public APIs, `StopToken` internally

**Why not `&dyn Stop`?** Can't clone it for thread fan-out. Can't own it.

**Why not `impl Stop + Clone + 'static`?** `StopToken::new()` doesn't need
Clone on T. The `'static` is the only bound needed — StopToken handles
Clone via Arc internally. Adding Clone to the public bound unnecessarily
rejects `BoxedStop`, `StopSource`, and `FnStop` with non-Clone closures.

**Why not `impl Stop` (no `'static`)?** StopToken requires `'static` for
the Arc. Use bare `impl Stop` for embedded/no_std code that accepts
`StopRef<'a>`.

### 2. `StopToken` uses `Option<Arc<dyn Stop>>`, not `Arc<dyn Stop>`

When the wrapped type's `may_stop()` returns false (e.g., `Unstoppable`),
StopToken stores `None`. No Arc allocated. `check()` short-circuits to
`Ok(())` without any vtable dispatch.

`Option<Arc<dyn Stop>>` gets null-pointer optimization — same 16 bytes
as `Arc<dyn Stop>`. Verified with compile-time assertion.

**Benchmark result:** StopToken(Unstoppable) is within noise of fully-inlined
generic `impl Stop` in hot loops — the check is optimized away in both. For
`Stopper`, the dispatch path makes no meaningful difference on real codec
workloads (see "Benchmark-Driven Findings" below); the flattened Arc keeps
the always-`Some` path a single, perfectly-predicted branch.

### 3. Stopper uses `Arc<StopperInner>`, not `Arc<AtomicBool>`

`StopperInner` implements `Stop` directly, so `From<Stopper> for StopToken`
is zero-cost pointer widening — the existing Arc is reused, not
double-wrapped.

Before: `StopToken(Stopper)` = `Arc<Stopper{Arc<AtomicBool>}>` — 2 hops.
After: `StopToken(Stopper)` = `Arc<StopperInner{AtomicBool}>` — 1 hop.
Same heap allocation, same `AtomicBool`, same memory address. All clones
(Stopper handles + StopToken + DynStop) share one `AtomicBool`.

### 4. `may_stop()` on the trait, not `active_stop()` method

`may_stop()` returns `bool` — works for `?Sized` types, no lifetime
issues. The `Option` optimization is built into `StopToken` and
`BoxedStop` at construction time, not per-check.

For `&dyn Stop` without StopToken, use `stop.may_stop().then_some(stop)`
to get `Option<&dyn Stop>` which implements `Stop` (from `enough`).

We tried `active_stop() -> Option<&dyn Stop>` as a trait method but
`Some(self)` doesn't compile for `?Sized` types (can't coerce `&Self`
to `&dyn Stop` generically).

### 5. `Unstoppable` is explicit, not hidden

No `decode()` + `decode_with()` pattern. One function: callers pass
`Unstoppable` explicitly. This makes the cancellation decision visible
in code rather than hidden behind a convenience wrapper.

### 6. `StopToken` not `DynStop`

Named for future migration: when StopToken moves from `almost-enough`
to `enough`, downstream code changes one import path. No renames.
`Stopper` creates and controls, `StopToken` is the handle you pass around.

### 7. `#![forbid(unsafe_code)]` on both crates

`StopperInner` wraps `AtomicBool` which is already `Send + Sync`.
No manual unsafe impls needed. `enough-ffi` retains unsafe (necessary
for FFI).

### 8. Relaxed ordering default, Acquire/Release opt-in

`Stopper` uses `Ordering::Relaxed` — fastest on ARM, sufficient for
"just stop." `SyncStopper` uses Release/Acquire for data-handoff
scenarios. SeqCst rejected as overkill for cancellation.

### 9. `Clone` is NOT on `Stop`

`Clone` requires `Sized`. `trait Stop: Clone` would make `dyn Stop`
impossible, killing the entire type-erasure story (`&dyn Stop`,
`StopToken`, `BoxedStop`). The `Clone` capability lives on `StopToken`
(via Arc) rather than on the trait.

### 10. `'static` is NOT on `Stop`

Would exclude `StopRef<'a>` — the zero-alloc borrowed type for
embedded/no_std. Keeping `Stop` minimal (`Send + Sync` only) maximizes
what it can accept. `'static` is required at the StopToken boundary.

## Benchmark-Driven Findings

### For real codec work, the dispatch path doesn't matter

The layout-immune benchmark routes every variant through one
`#[inline(never)] fn decode(&dyn Stop)`, so the measurement isn't biased
by where each variant's code happens to land. On that benchmark all
variants — generic `impl Stop`, `StopToken(Stopper)`, `StopToken(Unstoppable)`,
and `Option<&dyn Stop>` — sit within noise of each other. For the kind of
inner-loop check a codec actually does (check every N iterations against a
token that stays hot in L1), the choice between monomorphized generic and
type-erased `StopToken` is not a performance decision. Pick whichever is
ergonomic; `StopToken` is the recommended all-around handle for its
flatter type and `Clone`-for-fan-out.

> The micro hot-loop benchmarks (`hot_loop_stopper`, `hot_loop_unstoppable`)
> put each variant in its own closure, so their relative ordering is sensitive
> to code layout (Mytkowicz et al., ASPLOS 2009) and can flip between runs.
> Don't read a winner out of small µs deltas there — the layout-immune
> codec benchmark above is the one to trust.

### `impl Stop` inlining advantage is negligible

For `Unstoppable`, the compiler eliminates the check entirely whether it's
behind a generic `impl Stop` or a `StopToken` — both compile to the same
do-nothing path.

### `may_stop().then_some()` matches StopToken for Unstoppable

`Option<&dyn Stop> = None` matches generic and StopToken: the `None`
discriminant branch is perfectly predicted — effectively zero cost. This
is the `enough`-only optimization path.

### WithTimeout dominates all other costs

`WithTimeout.check()` at ~16ns is dominated by `Instant::now()`. All
other Stop types are <1ns. If you need timeouts, the Stop check
overhead is irrelevant — the clock read is 10-100x more expensive.

### Cache firewall changes results significantly

With zenbench's 2 MiB cache firewall, pointer-chasing benchmarks
(BoxedStop, StopToken) pay cache-miss costs. With firewall off (matching
real hot-loop behavior where the token stays in L1), all measurements
converge. Default should be firewall off for hot-path benchmarks.

## Type Overview

| Type | Size | check() | Clone | Alloc | Notes |
|------|------|---------|-------|-------|-------|
| `Unstoppable` | 0 | 0ns | Copy | none | Optimized away everywhere |
| `StopSource` | 1 byte | ~0.4ns | no | stack | Owns AtomicBool |
| `StopRef<'a>` | 8 bytes | ~0.4ns | Copy | none | Borrowed from StopSource |
| `Stopper` | 8 bytes | ~0.3ns | yes | Arc | Default choice |
| `SyncStopper` | 8 bytes | ~0.3ns | yes | Arc | Acquire/Release |
| `StopToken` | 16 bytes | 0ns/~1ns | yes | Arc/None | Recommended internal type |
| `BoxedStop` | 16 bytes | 0ns/~1ns | no | Box/None | Legacy, prefer StopToken |
| `ChildStopper` | 8 bytes | 1-3ns | yes | Arc | Walks parent chain |
| `WithTimeout<T>` | T + 16 | ~16ns | if T | if T | Instant::now() dominates |

## Future Direction

- **StopToken → `enough`**: Zero-migration rename. Library authors get
  erased + clonable without the `almost-enough` dep.
- **ClonableStop**: May move to `enough` if trait aliases stabilize.
- **`as_flag() -> Option<&AtomicBool>`**: Considered for bypassing
  vtable dispatch entirely. Rejected for now — leaks implementation.

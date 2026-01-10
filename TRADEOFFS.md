# Design Tradeoffs: `enough` API

This document captures the design decisions and tradeoffs considered when simplifying the `enough` cooperative cancellation API.

## Goals

1. **Library authors only need `Stop` trait** - one trait, `impl Stop`, done
2. **Default type is TINY** - inlines to ~3 instructions, no hierarchy overhead
3. **Sync-safe variant exists** - Release/Acquire ordering for data handoff scenarios
4. **Zero-alloc option remains** - for hot loops and `no_std`
5. **Hierarchy is opt-in** - separate type, don't pay for what you don't use

---

## 1. Stopper Size: Minimal vs Feature-Rich

### Option A: Tiny (Recommended)
```rust
pub struct Stopper {
    inner: Arc<AtomicBool>,  // 8 bytes, one pointer
}
```

**Pros:**
- `check()` is ~3 instructions: deref, atomic load, branch
- Struct is exactly one pointer (8 bytes)
- No option checking, no parent chain walking
- Compiler can inline aggressively
- Cache-friendly: single cache line access

**Cons:**
- No hierarchy support
- Users needing hierarchy must use different type
- No built-in timeout (but `WithTimeout` wrapper exists)

### Option B: Feature-Rich (Rejected)
```rust
pub struct Stopper {
    inner: Arc<StopperInner>,
}
struct StopperInner {
    cancelled: AtomicBool,
    parent: Option<Arc<StopperInner>>,  // Hierarchy support
    deadline: Option<Instant>,           // Built-in timeout
}
```

**Pros:**
- Single type does everything
- API is simpler (fewer types to learn)

**Cons:**
- `check()` is ~10-20 instructions minimum
- Always pays for Option checks even when not using hierarchy
- 32+ bytes per instance vs 8 bytes
- Parent chain walking is unbounded cost
- Harder for compiler to optimize

**Decision:** Option A. The 90% use case is "cancel this operation". Make that case as fast as possible. Users who need hierarchy can pay for it explicitly.

---

## 2. Memory Ordering: Relaxed vs Acquire/Release vs SeqCst

### The Core Question
When thread A calls `stop.cancel()` and thread B sees `stop.is_cancelled() == true`, what guarantees do we provide about other memory?

### Option A: Relaxed/Relaxed (Default Stopper)
```rust
pub fn cancel(&self) {
    self.inner.store(true, Ordering::Relaxed);
}
pub fn is_cancelled(&self) -> bool {
    self.inner.load(Ordering::Relaxed)
}
```

**Guarantees:** None beyond the cancellation flag itself.

**When this is fine:**
```rust
// Thread A
expensive_computation(&stop)?;  // Checks stop periodically
// Result is in return value, not shared memory

// Thread B
stop.cancel();  // Just tells A to stop
// Doesn't care about A's intermediate state
```

**Cost:** Minimal. On x86, Relaxed loads are free (same as regular loads).

### Option B: Release/Acquire (SyncStopper)
```rust
pub fn cancel(&self) {
    self.inner.store(true, Ordering::Release);
}
pub fn is_cancelled(&self) -> bool {
    self.inner.load(Ordering::Acquire)
}
```

**Guarantees:** All writes before `cancel()` are visible to readers after they observe `is_cancelled() == true`.

**When this matters:**
```rust
// Thread A (producer)
*shared_result = compute_result();  // Write result
sync_stop.cancel();                  // Release: flushes result

// Thread B (consumer)
if sync_stop.is_cancelled() {        // Acquire: syncs with release
    let r = *shared_result;          // GUARANTEED to see computed value
}
```

**Cost:** On x86, negligible. On ARM/weak memory models, adds memory barrier.

### Option C: SeqCst (Not Recommended)
```rust
pub fn cancel(&self) {
    self.inner.store(true, Ordering::SeqCst);
}
pub fn is_cancelled(&self) -> bool {
    self.inner.load(Ordering::SeqCst)
}
```

**Guarantees:** Total global ordering. All threads agree on the order of all SeqCst operations.

**When this matters:** Almost never for cancellation. Useful when you have multiple atomic variables that need consistent ordering across threads.

**Cost:** Highest. Full memory fence on all architectures.

**Decision:** Provide both Relaxed (`Stopper`) and Release/Acquire (`SyncStopper`). SeqCst is overkill for cancellation.

---

## 3. Hierarchy: Built-in vs Opt-in

### Option A: Opt-in Hierarchy (Recommended)
Separate type `ChildStopper` with explicit `.child()` method.

```rust
let parent = ChildStopper::new();
let child = parent.child();  // Explicit

parent.cancel();  // Cancels child too
```

**Pros:**
- Zero cost when not using hierarchy
- Clear in code when hierarchy is being used
- Each level adds predictable overhead

**Cons:**
- Another type to learn
- Can't add hierarchy to existing `Stopper` without changing types

### Option B: Built-in Hierarchy (Rejected)
Every stopper can have a parent.

```rust
let parent = Stopper::new();
let child = Stopper::child_of(&parent);  // Built into base type
```

**Pros:**
- One fewer type to learn
- Can convert between hierarchical/non-hierarchical

**Cons:**
- Every `Stopper` pays for `Option<Arc<...>>` even if not using hierarchy
- `check()` must always check if parent exists
- Harder to reason about performance

**Decision:** Option A. Hierarchy is a specialized feature. Don't penalize the common case.

---

## 4. Source/Token vs Unified Clone

### Option A: Unified Clone (Recommended for Arc types)
```rust
let stop = Stopper::new();
let stop2 = stop.clone();  // Both can cancel, both can check

stop2.cancel();
assert!(stop.is_cancelled());  // Both see it
```

**Pros:**
- Simpler mental model: "it's just a shared flag"
- Fewer types (no separate Token type)
- Matches tokio's CancellationToken design
- Natural for message passing: just send a clone

**Cons:**
- Any clone can cancel (no read-only tokens)
- Can't enforce "only owner cancels" at compile time

### Option B: Source/Token Split (Keep for zero-alloc)
```rust
let source = StopSource::new();  // On stack, owns the flag
let token = source.token();       // Borrowed reference

source.cancel();  // Only source can cancel
assert!(token.is_cancelled());
```

**Pros:**
- Clear ownership: source cancels, tokens observe
- Zero allocation for stack-based use
- Lifetime ensures token doesn't outlive source

**Cons:**
- Two types to understand
- Token is borrowed, can't be sent across threads without Arc

**Decision:** Both! `Stopper` uses unified clone (simple, Arc-based). `StopSource`/`StopRef` uses source/token split (zero-alloc, borrowed).

---

## 5. Error Type: Enum vs Struct vs Unit

### Option A: Enum (Current)
```rust
pub enum StopReason {
    Cancelled,
    TimedOut,
}
```

**Pros:**
- Caller can distinguish why stopped
- Useful for logging, metrics, error messages

**Cons:**
- 1 byte + alignment per error (8 bytes on 64-bit due to Result layout)

### Option B: Unit Struct
```rust
pub struct Stopped;
```

**Pros:**
- Zero-size type
- Simpler API

**Cons:**
- Can't distinguish cancellation from timeout
- Less informative error messages

### Option C: Struct with reason
```rust
pub struct Stopped {
    pub reason: StopReason,
}
```

**Pros:**
- Extensible (can add fields later)
- Can add backtrace, timestamp, etc.

**Cons:**
- More complex
- YAGNI for most use cases

**Decision:** Keep Option A (enum). The distinction between Cancelled and TimedOut is genuinely useful, and the size cost is minimal since errors are the exceptional path.

---

## 6. Trait Design: Minimal vs Rich

### Option A: Minimal Trait (Recommended)
```rust
pub trait Stop: Send + Sync {
    fn check(&self) -> Result<(), StopReason>;
}
```

**Pros:**
- Easy to implement
- Clear single responsibility
- Provided methods can add convenience (`is_stopped()`, `or()`)

**Cons:**
- Default `is_stopped()` makes extra method call (optimizes away)

### Option B: Rich Trait
```rust
pub trait Stop: Send + Sync {
    fn check(&self) -> Result<(), StopReason>;
    fn is_stopped(&self) -> bool;
    fn or<S: Stop>(self, other: S) -> Combined<Self, S>;
    fn with_timeout(self, dur: Duration) -> WithTimeout<Self>;
    // ...
}
```

**Pros:**
- More functionality in trait
- Implementors can optimize each method

**Cons:**
- More methods to implement
- Harder to implement correctly
- Most implementations would just use defaults anyway

**Decision:** Option A with provided methods. `check()` is the only required method. Everything else is provided with reasonable defaults.

---

## 7. Naming: Verbose vs Terse

### Type Names

| Verbose | Terse | Chosen |
|---------|-------|--------|
| `CancellationToken` | `Stop` | `Stopper` |
| `SynchronizedCancellationToken` | `SyncStop` | `SyncStopper` |
| `HierarchicalCancellationToken` | `TreeStop` | `ChildStopper` |
| `CancellationSource` | `Source` | `StopSource` |
| `CancellationRef` | `Ref` | `StopRef` |

**Decision:** Middle ground. Clear but not verbose. `Stopper` is the verb form of "stop", matches the `Stop` trait.

### Method Names

| Verbose | Terse | Chosen |
|---------|-------|--------|
| `is_cancellation_requested()` | `check()` | `check()` |
| `should_stop()` | `stop?` | `is_stopped()` |
| `request_cancellation()` | `cancel()` | `cancel()` |

**Decision:** Terse. These methods are called frequently in hot loops.

---

## 8. Feature Flags: Granular vs Coarse

### Option A: Granular (Current)
```toml
[features]
default = []
alloc = []
std = ["alloc"]
```

**Pros:**
- Maximum flexibility
- Can use in `no_std` environments
- Can use with only `alloc` (no std)

**Cons:**
- Users must think about features
- Documentation must cover all combinations

### Option B: Coarse
```toml
[features]
default = ["std"]
std = []
```

**Pros:**
- Simpler for most users
- Less documentation burden

**Cons:**
- Can't use in `no_std` with `alloc`
- Less flexible

**Decision:** Keep Option A. The `no_std` use case is real (embedded, WASM, kernel modules). Feature flags are a one-time cost.

---

## 9. Arc Overhead: Accept vs Optimize

### The Cost of Arc
```rust
pub struct Stopper {
    inner: Arc<AtomicBool>,
}
```

Every `Stopper::new()` does:
1. Allocate ~32 bytes (AtomicBool + strong/weak counts + padding)
2. Initialize counts
3. Return pointer

Every `clone()` does:
1. Atomic increment of strong count

Every `drop()` does:
1. Atomic decrement of strong count
2. If zero: deallocate

### Alternatives Considered

**A: Inline AtomicBool + Clone semantics**
Not possible - cloning would copy the bool, not share it.

**B: Global allocator pool**
Pre-allocate stoppers, reuse. Adds complexity, marginal benefit.

**C: Bump allocator**
For short-lived stoppers. Adds dependency, complexity.

**Decision:** Accept Arc overhead. The allocation happens once per logical "stop signal". The ongoing cost (atomic loads in `check()`) is minimal. Premature optimization would complicate the API.

---

## 10. Combinators: Trait Method vs Free Function vs Extension Trait

### How to combine stops?
```rust
// Want: stop if A OR B says stop
let combined = a.or(b);
```

### Option A: Trait Method (Current)
```rust
pub trait Stop {
    fn or<S: Stop>(self, other: S) -> OrStop<Self, S>
    where Self: Sized;
}
```

**Pros:**
- Discoverable via autocomplete
- Natural chaining: `a.or(b).or(c)`

**Cons:**
- Requires `Sized` bound or separate extension trait
- Adds to trait surface

### Option B: Free Function
```rust
pub fn any_of<A: Stop, B: Stop>(a: A, b: B) -> AnyOf<A, B>;
```

**Pros:**
- Clean trait
- Works with unsized types

**Cons:**
- Less discoverable
- Awkward chaining: `any_of(any_of(a, b), c)`

### Option C: Extension Trait
```rust
pub trait StopExt: Stop + Sized {
    fn or<S: Stop>(self, other: S) -> OrStop<Self, S>;
}
impl<T: Stop + Sized> StopExt for T {}
```

**Pros:**
- Keeps core trait minimal
- Full functionality via extension
- Clear separation of concerns

**Cons:**
- Another trait to import
- Slight indirection

**Decision:** Option A, method on trait. The `Sized` bound is acceptable since unsized Stop impls are rare. Ergonomics wins.

---

## Summary Table

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Default type size | Tiny (8 bytes) | Performance is king for hot loops |
| Memory ordering | Relaxed default, Acquire/Release opt-in | Most uses don't need sync guarantees |
| Hierarchy | Opt-in via `ChildStopper` | Don't pay for what you don't use |
| Source/Token | Unified for Arc, split for zero-alloc | Best of both worlds |
| Error type | Enum with 2 variants | Useful distinction, minimal cost |
| Trait design | Minimal required, rich provided | Easy to implement, powerful to use |
| Naming | Medium verbosity | Clear but not tedious |
| Features | Granular | Support all environments |
| Arc overhead | Accept it | Simple API over micro-optimization |
| Combinators | Trait method | Ergonomic chaining |

---

## Proposed Type Hierarchy

```
Stop (trait)
 │
 ├── Never                    // Zero-cost "never stop"
 │
 ├── Stopper                  // DEFAULT: tiny, just Arc<AtomicBool>
 │   └── .check() = 1 atomic load, ~3 instructions
 │
 ├── SyncStopper              // Release/Acquire ordering for data sync
 │   └── .check() = 1 atomic load with Acquire
 │   └── .cancel() = atomic store with Release
 │
 ├── ChildStopper              // Hierarchical (opt-in)
 │   └── .child() for hierarchy
 │   └── .check() walks parent chain
 │
 ├── StopSource / StopRef<'a> // Zero-alloc borrowed
 │
 └── WithTimeout<T>           // Deadline wrapper
```

## Performance Comparison

| Type | Size | check() cost | Ordering | Hierarchy |
|------|------|--------------|----------|-----------|
| `Never` | 0 | 0 (optimized away) | N/A | No |
| `Stopper` | 8 bytes | ~1-2ns | Relaxed | No |
| `SyncStopper` | 8 bytes | ~2-5ns | Acquire | No |
| `ChildStopper` | 8 bytes | ~5-20ns (chain walk) | Relaxed | Yes |
| `StopRef<'a>` | 8 bytes | ~1ns | Relaxed | No |

---

## Implemented Renames

These renames simplify the API by merging source/token for Arc types:

| Old Name | New Name | Rationale |
|----------|----------|-----------|
| `ArcStop` + `ArcToken` | `Stopper` | Unified clone model (like tokio) |
| `SyncStop` + `SyncToken` | `SyncStopper` | Release/Acquire ordering variant |
| `AtomicStop` | `StopSource` | Clearer: it's the source of the signal |
| `AtomicToken` | `StopRef` | Clearer: it's a reference/view |
| `ChildSource` + `ChildToken` | `ChildStopper` | Opt-in hierarchy, unified clone |
| `BoxStop` | `BoxedStop` | Consistent with Rust naming (`Boxed...`) |

### API Changes

| Old | New |
|-----|-----|
| `ArcStop::new()` | `Stopper::new()` |
| `source.token()` | `stop.clone()` |
| `ChildSource::new(parent.token())` | `ChildStopper::new()` or `parent.child()` |
| `AtomicStop::new()` | `StopSource::new()` |
| `source.token()` (AtomicToken) | `source.as_ref()` (StopRef) |

### Status

**IMPLEMENTED.** The renames above are now live in the crates.

---

## Implemented in `almost-enough`

### Type Erasure Helper (`into_boxed()`)

**Problem:** Users want to eliminate monomorphization at API boundaries without borrowing.

**Solution:** `StopExt::into_boxed()` method in `almost-enough` crate.

```rust
use almost_enough::{Stopper, BoxedStop, Stop, StopExt};

fn process(stop: impl Stop + 'static) {
    inner_work(stop.into_boxed());  // Single inner_work() impl
}

fn inner_work(stop: BoxedStop) {
    // Only one version of this function
}
```

**Status:** Implemented. Requires `'static` bound.

---

### Hierarchical Cancellation via `.child()`

**Problem:** Users want to create child stoppers from any `Stop` implementation.

**Solution:** `StopExt::child()` method in `almost-enough` crate.

```rust
use almost_enough::{Stopper, Stop, StopExt};

let parent = Stopper::new();
let child = parent.child();  // ChildStopper

parent.cancel();
assert!(child.should_stop());
```

**Status:** Implemented. Requires `Clone + 'static` bound.

---

### Drop Guard for Automatic Cancellation

**Problem:** Want RAII-style cancellation that triggers on scope exit.

**Solution:** `CancelGuard<C>` type and `StopDropRoll` trait in `almost-enough` crate.

```rust
use almost_enough::{Stopper, StopDropRoll};

fn work(source: &Stopper) -> Result<(), Error> {
    let guard = source.stop_on_drop();
    do_work()?;
    guard.disarm();  // Don't stop if we succeed
    Ok(())
}  // Stopped automatically if we exit early
```

**Naming:**
- `StopDropRoll` - memorable play on "stop, drop, and roll"
- `stop_on_drop()` - creates the guard
- `disarm()` - prevents stopping
- `Cancellable::stop()` - aligns with `Stop` trait, avoids conflict with `cancel()`

**Features:**
- Works with `Stopper` and `ChildStopper`
- `Cancellable` trait for extensibility
- `guard.is_armed()` to check state
- `guard.source()` to access underlying source

**Status:** Implemented.

---

## Crate Split: `enough` vs `almost-enough`

**Goal:** Keep `.or()` combinator ergonomic without polluting the core trait.

**Decision:** Extension trait in `almost-enough`.

**Current structure:**
- **`enough`** - Core trait and all implementations
  - `Stop` trait with `check()` and `should_stop()` only
  - `StopReason` enum
  - `Never`, `StopSource`, `StopRef`, `Stopper`, `SyncStopper`, `ChildStopper`, `BoxedStop`, `FnStop`, `OrStop`
  - `TimeoutExt`, `WithTimeout`
  - Blanket impls for `&T`, `&mut T`, `Box<T>`, `Arc<T>`

- **`almost-enough`** - Re-exports `enough` plus ergonomic extensions
  - `StopExt` extension trait with `.or()`, `.into_boxed()`, `.child()` methods
  - `StopDropRoll` trait with `.stop_on_drop()` for RAII guards
  - Re-exports everything from `enough`

**Usage:**
```rust
// Library authors: just depend on `enough`
use enough::{Stop, StopReason};
pub fn process(data: &[u8], stop: impl Stop) -> Result<(), StopReason> { ... }

// Application authors: use `almost-enough` for ergonomic combinators
use almost_enough::{StopSource, Stop, StopExt};
let combined = source_a.as_ref().or(source_b.as_ref());
```

**Rationale:** Library authors only need to accept `impl Stop`. Application authors get ergonomic `.or()` chaining via `almost-enough`. The extension trait pattern keeps the core trait minimal while providing full ergonomics for those who want it.

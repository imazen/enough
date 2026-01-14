# Reconciliation: `enough` vs Research Findings

## What `enough` Already Does Well

| Feature | Status | Notes |
|---------|--------|-------|
| `no_std` core | ✅ | Zero deps, `Stop` trait + `Unstoppable` work everywhere |
| Zero-cost `Unstoppable` | ✅ | `#[inline(always)]`, optimizes away |
| Borrowed tokens | ✅ | `AtomicStop`/`AtomicToken` - no allocation |
| Owned tokens | ✅ | `ArcStop`/`ArcToken` - heap allocated |
| Hierarchical | ✅ | `ChildSource`/`ChildToken` with parent propagation |
| Timeouts | ✅ | `WithTimeout`, `TimeoutExt`, tightening semantics |
| Tokio bridge | ✅ | `enough-tokio::TokioStop` |
| FFI | ✅ | `enough-ffi` with Arc-based safety |
| Dynamic dispatch | ✅ | `BoxStop` avoids monomorphization |
| Result-based | ✅ | `check() -> Result<(), StopReason>` with `?` support |
| Reason distinction | ✅ | `StopReason::Cancelled` vs `TimedOut` |

## Issues Found

### 1. Memory Ordering - SHOULD FIX

Currently uses `Ordering::Relaxed` everywhere:

```rust
// atomic.rs:89
pub fn cancel(&self) {
    self.cancelled.store(true, Ordering::Relaxed);  // ← Should be Release
}

// atomic.rs:95
pub fn is_cancelled(&self) -> bool {
    self.cancelled.load(Ordering::Relaxed)  // ← Should be Acquire
}
```

**Problem:** `Relaxed` doesn't establish happens-before relationships. If thread A does work then cancels, thread B might see the cancellation but not the work results.

**Fix:**
```rust
pub fn cancel(&self) {
    self.cancelled.store(true, Ordering::Release);
}

pub fn is_cancelled(&self) -> bool {
    self.cancelled.load(Ordering::Acquire)
}
```

**Affected files:**
- `atomic.rs` - `AtomicStop`, `AtomicToken`
- `arc.rs` - `ArcStop`, `ArcToken` (probably)
- `children/mod.rs` - `ChildSource`, `ChildToken`
- `enough-ffi/src/lib.rs` - `CancellationState`

### 2. Missing Combinator - CONSIDER ADDING

No way to combine multiple stops (any-of semantics):

```rust
// cancel-this has CancelChain, tokio has select!
// enough has no combinator

// Would be nice:
let combined = stop_a.or(stop_b).or(&atomic_flag);
```

**Suggested addition:**

```rust
/// Stops if ANY inner stop says stop
pub struct Any<A, B> { a: A, b: B }

impl<A: Stop, B: Stop> Stop for Any<A, B> {
    fn check(&self) -> Result<(), StopReason> {
        self.a.check()?;
        self.b.check()
    }
}

pub trait StopExt: Stop + Sized {
    fn or<B: Stop>(self, other: B) -> Any<Self, B>;
}
```

### 3. Missing Drop Guard - CONSIDER ADDING

tokio-util has `drop_guard()` that cancels on drop:

```rust
let guard = token.drop_guard();
// When guard is dropped, token is cancelled
```

**Could add to `ArcStop`:**

```rust
impl ArcStop {
    /// Returns a guard that cancels this source when dropped.
    pub fn drop_guard(&self) -> DropGuard {
        DropGuard { source: self.clone() }
    }
}

pub struct DropGuard {
    source: ArcStop,
}

impl Drop for DropGuard {
    fn drop(&mut self) {
        self.source.cancel();
    }
}
```

### 4. Thread-Local Convenience - OUT OF SCOPE

cancel-this's `is_cancelled!()` macro is ergonomic for deep call stacks:

```rust
// cancel-this
fn deep_function() -> Cancellable<()> {
    is_cancelled!()?;  // No parameter needed
    Ok(())
}
```

**Assessment:** This is a different design philosophy. `enough` favors explicit token passing, which is:
- More explicit about data flow
- Easier to reason about in multi-threaded code
- No hidden global state

**Recommendation:** Don't add. Users who want this can use `cancel-this` alongside `enough` with the adapter pattern.

### 5. cancel-this Adapter Crate - CONSIDER

Could provide `enough-cancel-this` for bidirectional bridging:

```rust
// enough Stop → cancel-this CancellationTrigger
pub struct StopAsTrigger<S: Stop>(S);

// cancel-this trigger → enough Stop
pub struct TriggerAsStop<T: CancellationTrigger>(T);
```

**Assessment:** Low priority. Users can implement this themselves in ~20 lines.

### 6. Async-std/smol Bridge - CONSIDER

Currently only tokio bridge exists. Could add:
- `enough-async-std`
- `enough-smol`

**Assessment:** Lower priority than tokio. Add if users request.

## Recommended Changes

### Must Fix
1. **Memory ordering** - Change `Relaxed` to `Release`/`Acquire` pairs

### Should Add
2. **Combinator** - Add `Any<A, B>` and `.or()` extension
3. **Drop guard** - Add to `ArcStop`

### Consider Later
4. **cancel-this adapter** - If users request
5. **Other runtime bridges** - If users request

### Don't Add
6. **Thread-local** - Different philosophy, use cancel-this if needed
7. **Callbacks** - Adds complexity, out of scope
8. **Liveness monitoring** - Out of scope for minimal crate

## Code Changes

### Fix 1: Memory Ordering

```bash
# Files to update:
crates/enough/src/atomic.rs
crates/enough/src/arc.rs
crates/enough/src/children/mod.rs
crates/enough-ffi/src/lib.rs
```

Pattern:
- `store(..., Ordering::Relaxed)` → `store(..., Ordering::Release)`
- `load(Ordering::Relaxed)` → `load(Ordering::Acquire)`

### Addition 2: Combinator

Add to `crates/enough/src/lib.rs`:

```rust
/// Stops if either inner stop says stop.
pub struct Any<A, B> {
    a: A,
    b: B,
}

impl<A: Stop, B: Stop> Any<A, B> {
    /// Create a new combinator.
    pub fn new(a: A, b: B) -> Self {
        Self { a, b }
    }
}

impl<A: Stop, B: Stop> Stop for Any<A, B> {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        self.a.check()?;
        self.b.check()
    }
}

// Extension trait
pub trait StopExt: Stop + Sized {
    /// Combine with another stop - stops if either says stop.
    fn or<B: Stop>(self, other: B) -> Any<Self, B> {
        Any::new(self, other)
    }
}

impl<T: Stop> StopExt for T {}
```

### Addition 3: Drop Guard

Add to `crates/enough/src/arc.rs`:

```rust
/// A guard that cancels an [`ArcStop`] when dropped.
pub struct DropGuard {
    source: Option<ArcStop>,
}

impl DropGuard {
    /// Disarm the guard - it will not cancel on drop.
    pub fn disarm(&mut self) {
        self.source = None;
    }
}

impl Drop for DropGuard {
    fn drop(&mut self) {
        if let Some(source) = &self.source {
            source.cancel();
        }
    }
}

impl ArcStop {
    /// Create a guard that cancels this source when dropped.
    pub fn drop_guard(&self) -> DropGuard {
        DropGuard {
            source: Some(self.clone()),
        }
    }
}
```

## Summary

`enough` is well-designed and covers the core use cases. The main issues are:

1. **Memory ordering bug** - Should fix before publishing
2. **Missing combinator** - Nice to have for mixing sources
3. **Missing drop guard** - Nice to have for RAII patterns

The design philosophy of explicit token passing is sound and shouldn't change. Users who want thread-local convenience can use `cancel-this` with an adapter.

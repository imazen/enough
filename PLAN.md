# enough - Cooperative Cancellation for Rust

A minimal, `no_std`-first crate providing a shared trait for cooperative cancellation in long-running operations.

## Vision

Enable codec authors and library writers to support cancellation with minimal effort:
- One trait to implement or accept
- Works across FFI boundaries (C#, Python, etc.)
- Works with async runtimes (tokio, async-std)
- Works with parallel processing (rayon)
- Zero dependencies in core crate

## User Stories

### Codec Author (Primary User)

1. **As a codec author**, I want to accept a cancellation parameter so that callers can stop long-running encode/decode operations.
   ```rust
   use enough::Stop;

   pub fn decode_jpeg(data: &[u8], stop: impl Stop) -> Result<Image, JpegError> {
       for (i, mcu) in mcus.enumerate() {
           if i % 16 == 0 {
               stop.check()?;
           }
           decode_mcu(mcu)?;
       }
       Ok(image)
   }
   ```

2. **As a codec author**, I want a simple `From` impl for my error type so that `?` works naturally.
   ```rust
   impl From<StopReason> for JpegError {
       fn from(r: StopReason) -> Self { JpegError::Stopped(r) }
   }
   ```

3. **As a codec author**, I want a `Never` type for callers who don't need cancellation, with zero overhead.

4. **As a codec author**, I want my library to be `no_std` compatible while still supporting cancellation.

### Application Developer

5. **As an app developer**, I want to cancel operations after a timeout.
   ```rust
   let source = CancellationSource::new();
   source.cancel_after(Duration::from_secs(30));
   let result = codec.decode(&data, source.token());
   ```

6. **As an app developer**, I want to cancel operations when the user presses Ctrl+C.
   ```rust
   let source = CancellationSource::new();
   ctrlc::set_handler(move || source.cancel());
   ```

7. **As an app developer**, I want to pass cancellation through multiple layers without lifetime hassles.
   ```rust
   // Token is Copy - no lifetime issues
   fn process(data: &[u8], stop: CancellationToken) -> Result<Output, Error> {
       let decoded = decoder::decode(data, stop)?;  // Pass by value
       let encoded = encoder::encode(&decoded, stop)?;  // Use again
       Ok(encoded)
   }
   ```

### Parallel Processing

8. **As a developer using rayon**, I want to cancel parallel work across all threads.
   ```rust
   let source = CancellationSource::new();
   let stop = source.token();

   images.par_iter().map(|img| {
       decode_image(img, stop)  // Token is Copy, shared across threads
   }).collect()
   ```

9. **As a developer**, I want child cancellation - cancel a subtree without affecting siblings.
   ```rust
   let parent = CancellationSource::new();

   // Each child can be cancelled independently
   let child_a = ChildCancellationSource::new(parent.token());
   let child_b = ChildCancellationSource::new(parent.token());

   // Cancel child_a - child_b continues
   child_a.cancel();

   // Cancel parent - all children stop
   parent.cancel();
   ```

### Async/Tokio Integration

10. **As a tokio user**, I want to bridge tokio's CancellationToken to `enough::Stop`.
    ```rust
    use enough_tokio::TokioStop;

    let token = tokio_util::sync::CancellationToken::new();
    let stop = TokioStop::new(token.clone());

    // In blocking task
    tokio::task::spawn_blocking(move || {
        codec.decode(&data, stop)
    });
    ```

11. **As a tokio user**, I want to combine cancellation with async select.
    ```rust
    tokio::select! {
        result = do_work() => result,
        _ = stop.cancelled() => Err(Cancelled),
    }
    ```

### FFI Integration (C#/.NET)

12. **As a C# developer**, I want to pass a CancellationToken to Rust and have it respected.
    ```csharp
    var cts = new CancellationTokenSource();
    cts.CancelAfter(TimeSpan.FromSeconds(30));

    using var handle = NativeMethods.cancellation_create();
    cts.Token.Register(() => NativeMethods.cancellation_cancel(handle));

    var result = NativeMethods.decode(data, handle);
    ```

13. **As a Rust FFI author**, I want to expose cancellation without complex lifetime management.
    ```rust
    #[no_mangle]
    pub extern "C" fn cancellation_create() -> *mut CancellationSource { ... }

    #[no_mangle]
    pub extern "C" fn cancellation_cancel(ptr: *mut CancellationSource) { ... }

    #[no_mangle]
    pub extern "C" fn decode(
        data: *const u8,
        len: usize,
        cancel: *const CancellationSource,
    ) -> FfiResult { ... }
    ```

### Timeout Integration

14. **As a developer**, I want to add timeouts that tighten but never loosen.
    ```rust
    // Parent has 60s timeout
    let stop = source.token().with_timeout(Duration::from_secs(60));

    // Child step limited to 10s (or parent's remaining time, whichever is less)
    let step_stop = stop.with_timeout(Duration::from_secs(10));
    ```

15. **As a developer**, I want to distinguish between cancellation and timeout in my error handling.
    ```rust
    match stop.check() {
        Ok(()) => continue,
        Err(StopReason::Cancelled) => return Err(Error::UserCancelled),
        Err(StopReason::TimedOut) => return Err(Error::Timeout),
    }
    ```

### Callback/Progress Integration

16. **As a developer**, I want to combine progress reporting with cancellation.
    ```rust
    pub struct ProgressStop<F> {
        inner: CancellationToken,
        on_progress: F,
        progress: AtomicU64,
    }

    impl<F: Fn(u64) + Send + Sync> Stop for ProgressStop<F> {
        fn check(&self) -> Result<(), StopReason> {
            (self.on_progress)(self.progress.load(Ordering::Relaxed));
            self.inner.check()
        }
    }
    ```

## Crate Structure

```
enough/
├── Cargo.toml              # Workspace root
├── PLAN.md                 # This file
├── README.md               # Public documentation
├── LICENSE-MIT
├── LICENSE-APACHE
│
├── crates/
│   ├── enough/             # Core crate (no_std, zero deps)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs      # Stop trait, StopReason, Never
│   │       └── reason.rs   # StopReason enum
│   │
│   ├── enough-std/         # std implementations (CancellationSource, etc.)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── source.rs       # CancellationSource (owns AtomicBool)
│   │       ├── token.rs        # CancellationToken (Copy, ptr-based)
│   │       ├── child.rs        # ChildCancellationSource (tree cancellation)
│   │       ├── timeout.rs      # Deadline/timeout support
│   │       └── callback.rs     # Callback-based cancellation
│   │
│   ├── enough-tokio/       # Tokio integration
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   │
│   └── enough-ffi/         # FFI helpers
│       ├── Cargo.toml
│       └── src/lib.rs
│
└── tests/                  # Integration test crates
    ├── test-basic/         # Basic Stop trait usage
    ├── test-atomic/        # AtomicBool-based implementations
    ├── test-timeout/       # Timeout behavior
    ├── test-child/         # Child cancellation trees
    ├── test-rayon/         # Parallel processing
    ├── test-tokio/         # Tokio integration
    ├── test-ffi/           # FFI from C
    ├── test-ffi-dotnet/    # FFI from C#/.NET
    └── test-codec-mock/    # Simulated codec with cancellation
```

## Feature Flags

### `enough` (core)
- `default = []` - Pure `no_std`, zero dependencies
- `std` - Enables `std::error::Error` impl for `StopReason`

### `enough-std`
- `default = ["std"]`
- `std` - Full std support (always on for this crate)
- `parking_lot` - Use parking_lot for synchronization

### `enough-tokio`
- `default = []`
- Depends on `tokio-util` for CancellationToken

## API Design

### Core Trait (`enough`)

```rust
#![no_std]

/// Why an operation was stopped
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StopReason {
    /// Operation was explicitly cancelled
    Cancelled,
    /// Operation exceeded its deadline
    TimedOut,
}

/// Cooperative cancellation check
///
/// Implementors must be thread-safe (Send + Sync).
pub trait Stop: Send + Sync {
    /// Check if the operation should stop.
    ///
    /// Returns `Ok(())` to continue, `Err(reason)` to stop.
    fn check(&self) -> Result<(), StopReason>;

    /// Convenience: returns true if stopped
    #[inline]
    fn is_stopped(&self) -> bool {
        self.check().is_err()
    }
}

/// A Stop implementation that never stops (zero-cost)
#[derive(Debug, Clone, Copy, Default)]
pub struct Never;

impl Stop for Never {
    #[inline(always)]
    fn check(&self) -> Result<(), StopReason> {
        Ok(())
    }
}

// Blanket impls for references, Box, Arc
impl<T: Stop + ?Sized> Stop for &T { ... }
impl<T: Stop + ?Sized> Stop for &mut T { ... }

#[cfg(feature = "alloc")]
impl<T: Stop + ?Sized> Stop for alloc::boxed::Box<T> { ... }

#[cfg(feature = "alloc")]
impl<T: Stop + ?Sized> Stop for alloc::sync::Arc<T> { ... }
```

### Std Implementations (`enough-std`)

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Owns the cancellation state. Creates tokens.
pub struct CancellationSource {
    cancelled: AtomicBool,
}

impl CancellationSource {
    pub fn new() -> Self;
    pub fn cancel(&self);
    pub fn is_cancelled(&self) -> bool;
    pub fn token(&self) -> CancellationToken;
}

/// Copy token - checks the source's AtomicBool
#[derive(Clone, Copy)]
pub struct CancellationToken {
    ptr: *const AtomicBool,
    deadline: Option<Instant>,
}

impl CancellationToken {
    pub const fn never() -> Self;  // Null ptr = never cancelled
    pub fn with_timeout(self, duration: Duration) -> Self;
    pub fn with_deadline(self, deadline: Instant) -> Self;
}

impl Stop for CancellationToken { ... }

/// Child source - cancelled when parent OR self is cancelled
pub struct ChildCancellationSource {
    own_flag: AtomicBool,
    parent_flags: SmallVec<[*const AtomicBool; 4]>,
}
```

## Testing Strategy

### Unit Tests (in each crate)
- Stop trait basics
- Never type optimization
- StopReason equality/hash/display

### Integration Tests (test-* crates)

| Test Crate | What It Tests |
|------------|---------------|
| `test-basic` | Trait usage, Never, basic impl |
| `test-atomic` | AtomicBool-based Source/Token |
| `test-timeout` | Deadline behavior, tightening |
| `test-child` | Parent/child cancellation trees |
| `test-rayon` | Parallel iterator cancellation |
| `test-tokio` | Tokio bridge, async select |
| `test-ffi` | C FFI round-trip |
| `test-ffi-dotnet` | C# CancellationToken bridge |
| `test-codec-mock` | Simulated codec workload |

### Stress Tests
- Many concurrent cancellations
- Deep child trees
- Rapid cancel/check cycles
- Memory leak detection

## Implementation Phases

### Phase 1: Core Trait
- [ ] `enough` crate with `Stop`, `StopReason`, `Never`
- [ ] `no_std` support
- [ ] Blanket impls for references
- [ ] Basic tests

### Phase 2: Std Implementations
- [ ] `CancellationSource` and `CancellationToken`
- [ ] Timeout/deadline support
- [ ] Child cancellation
- [ ] Callback support

### Phase 3: Integrations
- [ ] `enough-tokio` - Tokio bridge
- [ ] `enough-ffi` - FFI helpers

### Phase 4: Test Suite
- [ ] All test-* crates
- [ ] Stress tests
- [ ] Documentation examples

### Phase 5: Polish
- [ ] README with examples
- [ ] API documentation
- [ ] Publish to crates.io
- [ ] GitHub repo setup

## Open Questions

1. **Should `StopReason` be `#[non_exhaustive]`?** - Yes, to allow future variants like `ResourceExhausted`

2. **Should we provide a derive macro for error integration?** - Probably not initially, `From` impl is one line

3. **Should `CancellationToken` be generic over the flag type?** - No, keep it simple with `AtomicBool`

4. **Should we integrate with `std::panic::catch_unwind`?** - Out of scope, panics are separate concern

## References

- [Error design blog post](https://fast.github.io/blog/stop-forwarding-errors-start-designing-them/)
- `cancel-this` crate (thread-local approach, different use case)
- `tokio-util::sync::CancellationToken`
- C# `CancellationToken` / `CancellationTokenSource`

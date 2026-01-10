//! Synchronized cancellation with memory ordering guarantees.
//!
//! This module provides [`SyncStop`], which uses Release/Acquire ordering
//! to ensure memory synchronization between the cancelling thread and
//! threads that observe the cancellation.
//!
//! # When to Use
//!
//! Use `SyncStop` when you need to ensure that writes made before `cancel()`
//! are visible to readers after they see `should_stop() == true`.
//!
//! ```rust
//! use enough::{SyncStop, Stop};
//! use std::sync::atomic::{AtomicUsize, Ordering};
//!
//! static SHARED_DATA: AtomicUsize = AtomicUsize::new(0);
//!
//! let stop = SyncStop::new();
//!
//! // Thread A: producer
//! SHARED_DATA.store(42, Ordering::Relaxed);
//! stop.cancel();  // Release: flushes SHARED_DATA write
//!
//! // Thread B: consumer
//! if stop.should_stop() {  // Acquire: syncs with Release
//!     // GUARANTEED to see SHARED_DATA == 42
//!     let value = SHARED_DATA.load(Ordering::Relaxed);
//!     assert_eq!(value, 42);
//! }
//! ```
//!
//! # When NOT to Use
//!
//! If you don't need synchronization guarantees (most cancellation use cases),
//! use [`AtomicStop`](crate::AtomicStop) instead - it's slightly faster on
//! weakly-ordered architectures (ARM, etc.).
//!
//! ```rust
//! use enough::{AtomicStop, Stop};
//!
//! // Just need to signal "stop" - no data synchronization needed
//! let stop = AtomicStop::new();
//! // ... use stop.cancel() and stop.should_stop() ...
//! ```
//!
//! # Memory Ordering
//!
//! | Operation | Ordering | Effect |
//! |-----------|----------|--------|
//! | `cancel()` | Release | Flushes prior writes |
//! | `is_cancelled()` | Acquire | Syncs with Release |
//! | `should_stop()` | Acquire | Syncs with Release |
//! | `check()` | Acquire | Syncs with Release |

use core::sync::atomic::{AtomicBool, Ordering};

use crate::{Stop, StopReason};

/// A cancellation source with Release/Acquire memory ordering.
///
/// Unlike [`AtomicStop`](crate::AtomicStop) which uses Relaxed ordering,
/// `SyncStop` guarantees that all writes before `cancel()` are visible
/// to any thread that subsequently observes `should_stop() == true`.
///
/// # Example
///
/// ```rust
/// use enough::{SyncStop, Stop};
///
/// let stop = SyncStop::new();
///
/// // In producer thread:
/// // ... write shared data ...
/// stop.cancel();  // Release barrier
///
/// // In consumer thread:
/// if stop.should_stop() {  // Acquire barrier
///     // Safe to read shared data written before cancel()
/// }
/// ```
///
/// # Performance
///
/// On x86/x64, Release/Acquire has negligible overhead (strong memory model).
/// On ARM and other weakly-ordered architectures, there's a small cost for
/// the memory barriers. Use [`AtomicStop`](crate::AtomicStop) if you don't
/// need the synchronization guarantees.
#[derive(Debug)]
pub struct SyncStop {
    cancelled: AtomicBool,
}

impl SyncStop {
    /// Create a new synchronized cancellation source.
    #[inline]
    pub const fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
        }
    }

    /// Create a source that is already cancelled.
    #[inline]
    pub const fn cancelled() -> Self {
        Self {
            cancelled: AtomicBool::new(true),
        }
    }

    /// Cancel with Release ordering.
    ///
    /// All memory writes before this call are guaranteed to be visible
    /// to any thread that subsequently observes `should_stop() == true`.
    #[inline]
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Check if cancelled with Acquire ordering.
    ///
    /// If this returns `true`, all memory writes that happened before
    /// the corresponding `cancel()` call are guaranteed to be visible.
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Get a token that can be passed to operations.
    ///
    /// The token borrows from this source and uses the same
    /// Acquire ordering for reads.
    #[inline]
    pub fn token(&self) -> SyncToken<'_> {
        SyncToken {
            cancelled: &self.cancelled,
        }
    }
}

impl Default for SyncStop {
    fn default() -> Self {
        Self::new()
    }
}

impl Stop for SyncStop {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }

    #[inline]
    fn should_stop(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// A borrowed token with Acquire ordering.
///
/// This is a lightweight reference to a [`SyncStop`]. It maintains the
/// same memory ordering guarantees as the source.
#[derive(Debug, Clone, Copy)]
pub struct SyncToken<'a> {
    cancelled: &'a AtomicBool,
}

impl Stop for SyncToken<'_> {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }

    #[inline]
    fn should_stop(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_stop_basic() {
        let source = SyncStop::new();
        assert!(!source.is_cancelled());
        assert!(!source.should_stop());
        assert!(source.check().is_ok());

        source.cancel();

        assert!(source.is_cancelled());
        assert!(source.should_stop());
        assert_eq!(source.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn sync_stop_cancelled_constructor() {
        let source = SyncStop::cancelled();
        assert!(source.is_cancelled());
        assert!(source.should_stop());
    }

    #[test]
    fn sync_token_basic() {
        let source = SyncStop::new();
        let token = source.token();

        assert!(!token.should_stop());
        assert!(token.check().is_ok());

        source.cancel();

        assert!(token.should_stop());
        assert_eq!(token.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn sync_token_is_copy() {
        let source = SyncStop::new();
        let t1 = source.token();
        let t2 = t1; // Copy
        let _ = t1; // Still valid
        let _ = t2;
    }

    #[test]
    fn sync_stop_is_default() {
        let source: SyncStop = Default::default();
        assert!(!source.is_cancelled());
    }

    #[test]
    fn sync_stop_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SyncStop>();
        assert_send_sync::<SyncToken<'_>>();
    }

    #[test]
    fn cancel_is_idempotent() {
        let source = SyncStop::new();
        source.cancel();
        source.cancel();
        source.cancel();
        assert!(source.is_cancelled());
    }

    #[cfg(feature = "std")]
    #[test]
    fn sync_ordering_guarantees() {
        use std::sync::atomic::AtomicUsize;

        let stop = SyncStop::new();
        let data = AtomicUsize::new(0);

        // This test verifies the ordering semantics compile correctly.
        // Actual ordering verification would require more complex testing
        // with tools like loom or ThreadSanitizer.

        // Producer
        data.store(42, Ordering::Relaxed);
        stop.cancel(); // Release

        // Consumer (same thread for simplicity)
        if stop.should_stop() {
            // Acquire
            let value = data.load(Ordering::Relaxed);
            assert_eq!(value, 42);
        }
    }
}

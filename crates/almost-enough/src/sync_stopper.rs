//! Synchronized cancellation with memory ordering guarantees.
//!
//! [`SyncStopper`] uses Release/Acquire ordering to ensure memory synchronization
//! between the cancelling thread and threads that observe the cancellation.
//!
//! # When to Use
//!
//! Use `SyncStopper` when you need to ensure that writes made before `cancel()`
//! are visible to readers after they see `should_stop() == true`.
//!
//! ```rust
//! use almost_enough::{SyncStopper, Stop};
//! use std::sync::atomic::{AtomicUsize, Ordering};
//!
//! static SHARED_DATA: AtomicUsize = AtomicUsize::new(0);
//!
//! let stop = SyncStopper::new();
//!
//! // Thread A: producer
//! SHARED_DATA.store(42, Ordering::Relaxed);
//! stop.cancel();  // Release: flushes SHARED_DATA write
//!
//! // Thread B: consumer (same thread here for demo)
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
//! use [`Stopper`](crate::Stopper) instead - it's slightly faster on
//! weakly-ordered architectures (ARM, etc.).
//!
//! # Memory Ordering
//!
//! | Operation | Ordering | Effect |
//! |-----------|----------|--------|
//! | `cancel()` | Release | Flushes prior writes |
//! | `is_cancelled()` | Acquire | Syncs with Release |
//! | `should_stop()` | Acquire | Syncs with Release |
//! | `check()` | Acquire | Syncs with Release |

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{Stop, StopReason};

/// Inner state for [`SyncStopper`] — implements [`Stop`] with Acquire ordering.
pub(crate) struct SyncStopperInner {
    cancelled: AtomicBool,
}

impl Stop for SyncStopperInner {
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

/// A cancellation primitive with Release/Acquire memory ordering.
///
/// Unlike [`Stopper`](crate::Stopper) which uses Relaxed ordering,
/// `SyncStopper` guarantees that all writes before `cancel()` are visible
/// to any clone that subsequently observes `should_stop() == true`.
///
/// Converts to [`StopToken`](crate::StopToken) via `From`/`Into` with zero
/// overhead — the existing `Arc` is reused, not double-wrapped.
///
/// # Performance
///
/// On x86/x64, Release/Acquire has negligible overhead (strong memory model).
/// On ARM and other weakly-ordered architectures, there's a small cost for
/// the memory barriers. Use [`Stopper`](crate::Stopper) if you don't
/// need the synchronization guarantees.
#[derive(Debug, Clone)]
pub struct SyncStopper {
    pub(crate) inner: Arc<SyncStopperInner>,
}

impl SyncStopper {
    /// Create a new synchronized stopper.
    #[inline]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(SyncStopperInner {
                cancelled: AtomicBool::new(false),
            }),
        }
    }

    /// Create a stopper that is already cancelled.
    #[inline]
    pub fn cancelled() -> Self {
        Self {
            inner: Arc::new(SyncStopperInner {
                cancelled: AtomicBool::new(true),
            }),
        }
    }

    /// Cancel with Release ordering.
    ///
    /// All memory writes before this call are guaranteed to be visible
    /// to any clone that subsequently observes `should_stop() == true`.
    #[inline]
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Release);
    }

    /// Check if cancelled with Acquire ordering.
    ///
    /// If this returns `true`, all memory writes that happened before
    /// the corresponding `cancel()` call are guaranteed to be visible.
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }
}

impl Default for SyncStopper {
    fn default() -> Self {
        Self::new()
    }
}

impl Stop for SyncStopper {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        self.inner.check()
    }

    #[inline]
    fn should_stop(&self) -> bool {
        self.inner.should_stop()
    }
}

impl core::fmt::Debug for SyncStopperInner {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SyncStopperInner")
            .field("cancelled", &self.cancelled.load(Ordering::Relaxed))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_stopper_basic() {
        let stop = SyncStopper::new();
        assert!(!stop.is_cancelled());
        assert!(!stop.should_stop());
        assert!(stop.check().is_ok());

        stop.cancel();

        assert!(stop.is_cancelled());
        assert!(stop.should_stop());
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn sync_stopper_cancelled_constructor() {
        let stop = SyncStopper::cancelled();
        assert!(stop.is_cancelled());
        assert!(stop.should_stop());
    }

    #[test]
    fn sync_stopper_clone_shares_state() {
        let stop1 = SyncStopper::new();
        let stop2 = stop1.clone();

        stop2.cancel();

        assert!(stop1.should_stop());
        assert!(stop2.should_stop());
    }

    #[test]
    fn sync_stopper_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SyncStopper>();
    }

    #[test]
    fn sync_stopper_is_default() {
        let stop: SyncStopper = Default::default();
        assert!(!stop.is_cancelled());
    }
}

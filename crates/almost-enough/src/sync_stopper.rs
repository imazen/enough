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
//! It costs no more than [`Stopper`](crate::Stopper) measurably, so it is
//! the default choice; see the type's docs. Use `Stopper` only when the flag
//! carries nothing but "stop".
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
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }

    #[inline]
    #[track_caller]
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
/// This is the default choice: if you're not sure which stop to use, use
/// this one.
///
/// # When the ordering matters
///
/// When the cancelling thread hands over data through Relaxed atomics, such
/// as a reason code stored just before `cancel()`. Data behind a `Mutex`,
/// sent on a channel, or read after a `join` is synchronized anyway. On ARM
/// the difference is real: in a litmus test (`examples/stop_ordering.rs`), a
/// reader that had seen a [`Stopper`](crate::Stopper)'s stop still read the
/// old value in 21% of rounds on a Neoverse-N1 and in 1 of 10,500 on an Apple
/// M4 Pro. With `SyncStopper` it never did, on either. x86-64 can't reorder
/// that way, so neither type ever read stale there.
///
/// # Performance
///
/// On x86-64 a check compiles to the same instructions as `Stopper`'s (an
/// Acquire load is an ordinary load). On aarch64 it is one more instruction
/// (`ldaprb` takes no address offset). zenbench measured no difference on an
/// Apple M4 Pro, isolated or in a loop that checks every 64 bytes. On a
/// Neoverse-N1 it measured none in isolation and +2–3% in that 64-byte loop.
/// Use `Stopper` only when the flag carries nothing but "stop".
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
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.inner.check()
    }

    #[inline]
    #[track_caller]
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

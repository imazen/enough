//! The default cancellation primitive.
//!
//! [`Stopper`] is the recommended type for most use cases. It's a simple,
//! Arc-based cancellation flag with unified clone semantics.
//!
//! # Example
//!
//! ```rust
//! use enough::{Stopper, Stop};
//!
//! let stop = Stopper::new();
//! let stop2 = stop.clone();  // Both share the same flag
//!
//! assert!(!stop.should_stop());
//!
//! stop2.cancel();  // Any clone can cancel
//! assert!(stop.should_stop());
//! ```
//!
//! # Design
//!
//! `Stopper` uses a unified clone model (like tokio's `CancellationToken`):
//! - Just clone to share - no separate "token" type
//! - Any clone can call `cancel()`
//! - Any clone can check `should_stop()`
//!
//! This is simpler than source/token split but means you can't prevent
//! a recipient from cancelling. If you need that, use
//! [`StopSource`](crate::StopSource)/[`StopRef`](crate::StopRef).
//!
//! # Memory Ordering
//!
//! Uses Relaxed ordering for best performance. If you need to synchronize
//! other memory writes with cancellation, use [`SyncStopper`](crate::SyncStopper).

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{Stop, StopReason};

/// A cancellation primitive with unified clone semantics.
///
/// This is the recommended default for most use cases. Clone it to share
/// the cancellation state - any clone can cancel or check status.
///
/// # Example
///
/// ```rust
/// use enough::{Stopper, Stop};
///
/// let stop = Stopper::new();
///
/// // Pass a clone to another thread
/// let stop2 = stop.clone();
/// std::thread::spawn(move || {
///     while !stop2.should_stop() {
///         // do work
///         break;
///     }
/// }).join().unwrap();
///
/// // Cancel from original
/// stop.cancel();
/// ```
///
/// # Performance
///
/// - Size: 8 bytes (one pointer)
/// - `check()`: ~1-2ns (single atomic load with Relaxed ordering)
/// - `clone()`: atomic increment
/// - `cancel()`: atomic store
#[derive(Debug, Clone)]
pub struct Stopper {
    cancelled: Arc<AtomicBool>,
}

impl Stopper {
    /// Create a new stopper.
    #[inline]
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Create a stopper that is already cancelled.
    ///
    /// Useful for testing or when you want to signal immediate stop.
    #[inline]
    pub fn cancelled() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Signal all clones to stop.
    ///
    /// This is idempotent - calling it multiple times has no additional effect.
    #[inline]
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Check if cancellation has been requested.
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

impl Default for Stopper {
    fn default() -> Self {
        Self::new()
    }
}

impl Stop for Stopper {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }

    #[inline]
    fn should_stop(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopper_basic() {
        let stop = Stopper::new();
        assert!(!stop.is_cancelled());
        assert!(!stop.should_stop());
        assert!(stop.check().is_ok());

        stop.cancel();

        assert!(stop.is_cancelled());
        assert!(stop.should_stop());
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn stopper_cancelled_constructor() {
        let stop = Stopper::cancelled();
        assert!(stop.is_cancelled());
        assert!(stop.should_stop());
    }

    #[test]
    fn stopper_clone_shares_state() {
        let stop1 = Stopper::new();
        let stop2 = stop1.clone();

        assert!(!stop1.should_stop());
        assert!(!stop2.should_stop());

        // Either clone can cancel
        stop2.cancel();

        assert!(stop1.should_stop());
        assert!(stop2.should_stop());
    }

    #[test]
    fn stopper_is_default() {
        let stop: Stopper = Default::default();
        assert!(!stop.is_cancelled());
    }

    #[test]
    fn stopper_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Stopper>();
    }

    #[test]
    fn stopper_can_outlive_original() {
        let stop2 = {
            let stop1 = Stopper::new();
            stop1.clone()
        };
        // Original is dropped, but clone still works
        assert!(!stop2.should_stop());
    }

    #[test]
    fn cancel_is_idempotent() {
        let stop = Stopper::new();
        stop.cancel();
        stop.cancel();
        stop.cancel();
        assert!(stop.is_cancelled());
    }
}

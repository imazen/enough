//! [`RelaxedStopper`]: a [`Stopper`](crate::Stopper) without the visibility
//! guarantee.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{Stop, StopReason};

/// A [`RelaxedStopper`]'s flag. It implements `Stop` itself, so a
/// [`StopToken`](crate::StopToken) can hold it behind the vtable without
/// another `Arc`.
pub(crate) struct RelaxedStopperInner {
    cancelled: AtomicBool,
}

impl Stop for RelaxedStopperInner {
    #[inline]
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }

    #[inline]
    #[track_caller]
    fn should_stop(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// A [`Stopper`](crate::Stopper) whose flag is stored and checked with
/// Relaxed ordering: what `Stopper` was before 0.4.5.
///
/// It stops when a `Stopper` would, and as soon: the ordering doesn't change
/// how soon another core sees `cancel()`. It drops one guarantee. Code that
/// sees the stop may still read stale values of other atomics the cancelling
/// thread wrote before `cancel()`, such as a reason code kept in an atomic.
/// On ARM that happens (x86-64 can't reorder that way). Use it only where
/// nothing is handed over through the cancellation; data behind a `Mutex`,
/// sent on a channel or read after a `join` is synchronized anyway.
///
/// The only measured gain is on aarch64 targets without RCpc (Linux,
/// Windows, Android and iOS builds), where `Stopper`'s Acquire load is
/// `ldar`. On a Neoverse-N1, a compute-bound loop checking every 64 bytes
/// through `&dyn Stop` ran 3–10% faster than with `Stopper`. Checking every
/// KiB, loops bound by memory latency, x86-64 and macOS builds showed no
/// difference. See `benchmarks/stopper-ordering-2026-10-06.md`.
///
/// Pass it as `&dyn Stop` or generically. A [`StopToken`](crate::StopToken)
/// reuses its `Arc` but checks it through the vtable, which costs more than
/// the token's direct path for a `Stopper`.
///
/// # Example
///
/// ```rust
/// use almost_enough::{RelaxedStopper, Stop};
///
/// let stop = RelaxedStopper::new();
/// let worker = stop.clone();
/// assert!(!worker.should_stop());
/// stop.cancel();
/// assert!(worker.should_stop());
/// ```
#[derive(Debug, Clone)]
pub struct RelaxedStopper {
    pub(crate) inner: Arc<RelaxedStopperInner>,
}

impl RelaxedStopper {
    /// Create a new stopper.
    #[inline]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RelaxedStopperInner {
                cancelled: AtomicBool::new(false),
            }),
        }
    }

    /// Create a stopper that is already cancelled.
    #[inline]
    pub fn cancelled() -> Self {
        Self {
            inner: Arc::new(RelaxedStopperInner {
                cancelled: AtomicBool::new(true),
            }),
        }
    }

    /// Signal all clones to stop. A Relaxed store; idempotent.
    #[inline]
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Relaxed);
    }

    /// Check if cancellation has been requested, with Relaxed ordering.
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Relaxed)
    }
}

impl Default for RelaxedStopper {
    fn default() -> Self {
        Self::new()
    }
}

impl Stop for RelaxedStopper {
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

impl core::fmt::Debug for RelaxedStopperInner {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RelaxedStopperInner")
            .field("cancelled", &self.cancelled.load(Ordering::Relaxed))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_the_flag() {
        let stop = RelaxedStopper::new();
        let clone = stop.clone();
        assert!(!clone.is_cancelled());
        assert_eq!(clone.check(), Ok(()));
        stop.cancel();
        assert!(clone.is_cancelled());
        assert!(clone.should_stop());
        assert_eq!(clone.check(), Err(StopReason::Cancelled));
        stop.cancel();
        assert!(stop.is_cancelled());
    }

    #[test]
    fn cancelled_and_default() {
        assert!(RelaxedStopper::cancelled().should_stop());
        assert!(!RelaxedStopper::default().should_stop());
        assert!(RelaxedStopper::new().may_stop());
    }

    #[test]
    fn debug_shows_the_flag() {
        let stop = RelaxedStopper::cancelled();
        assert!(alloc::format!("{stop:?}").contains("cancelled: true"));
    }

    #[test]
    fn is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RelaxedStopper>();
    }
}

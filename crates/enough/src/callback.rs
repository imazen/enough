//! Callback-based cancellation.
//!
//! This module requires the `std` feature.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::{Stop, StopReason};

/// Inner state for callback cancellation.
struct CallbackInner<F> {
    cancelled: AtomicBool,
    callback: F,
}

/// A cancellation source that triggers a callback when cancelled.
///
/// Useful for integrating with external cancellation systems.
///
/// # Example
///
/// ```rust
/// use enough::{CallbackCancellation, Stop};
/// use std::sync::atomic::{AtomicBool, Ordering};
/// use std::sync::Arc;
///
/// let notified = Arc::new(AtomicBool::new(false));
/// let notified_clone = notified.clone();
///
/// let source = CallbackCancellation::new(move || {
///     notified_clone.store(true, Ordering::SeqCst);
/// });
///
/// assert!(!notified.load(Ordering::SeqCst));
///
/// source.cancel();
///
/// assert!(notified.load(Ordering::SeqCst));
/// assert!(source.token().is_stopped());
/// ```
pub struct CallbackCancellation<F: Fn() + Send + Sync> {
    inner: Arc<CallbackInner<F>>,
}

impl<F: Fn() + Send + Sync> CallbackCancellation<F> {
    /// Create a new callback cancellation source.
    ///
    /// The callback will be invoked when [`cancel()`](Self::cancel) is called.
    pub fn new(callback: F) -> Self {
        Self {
            inner: Arc::new(CallbackInner {
                cancelled: AtomicBool::new(false),
                callback,
            }),
        }
    }

    /// Cancel and invoke the callback.
    ///
    /// The callback is invoked exactly once, on the first call to cancel.
    pub fn cancel(&self) {
        if !self.inner.cancelled.swap(true, Ordering::AcqRel) {
            (self.inner.callback)();
        }
    }

    /// Check if cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    /// Get a token for this source.
    pub fn token(&self) -> CallbackCancellationToken<F> {
        CallbackCancellationToken {
            inner: Arc::clone(&self.inner),
        }
    }
}

/// Token for callback-based cancellation.
pub struct CallbackCancellationToken<F: Fn() + Send + Sync> {
    inner: Arc<CallbackInner<F>>,
}

impl<F: Fn() + Send + Sync> Clone for CallbackCancellationToken<F> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<F: Fn() + Send + Sync> Stop for CallbackCancellationToken<F> {
    fn check(&self) -> Result<(), StopReason> {
        if self.inner.cancelled.load(Ordering::Acquire) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_invoked_on_cancel() {
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let source = CallbackCancellation::new(move || {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });

        assert_eq!(counter.load(Ordering::SeqCst), 0);

        source.cancel();

        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn callback_invoked_only_once() {
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let source = CallbackCancellation::new(move || {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        });

        source.cancel();
        source.cancel();
        source.cancel();

        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn token_reflects_state() {
        let source = CallbackCancellation::new(|| {});
        let token = source.token();

        assert!(!token.is_stopped());

        source.cancel();

        assert!(token.is_stopped());
    }
}

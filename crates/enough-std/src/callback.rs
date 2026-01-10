//! Callback-based cancellation.

use std::sync::atomic::{AtomicBool, Ordering};

use enough::{Stop, StopReason};

/// A cancellation source that triggers a callback when cancelled.
///
/// Useful for integrating with external cancellation systems that
/// need notification when cancellation occurs.
///
/// # Example
///
/// ```rust
/// use enough_std::CallbackCancellation;
/// use enough::Stop;
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
    cancelled: AtomicBool,
    callback: F,
}

impl<F: Fn() + Send + Sync> CallbackCancellation<F> {
    /// Create a new callback cancellation source.
    ///
    /// The callback will be invoked when [`cancel()`](Self::cancel) is called.
    pub fn new(callback: F) -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            callback,
        }
    }

    /// Cancel and invoke the callback.
    ///
    /// The callback is invoked exactly once, on the first call to cancel.
    /// Subsequent calls are no-ops.
    pub fn cancel(&self) {
        // Only invoke callback on first cancellation
        if !self.cancelled.swap(true, Ordering::AcqRel) {
            (self.callback)();
        }
    }

    /// Check if cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Get a token for this source.
    pub fn token(&self) -> CallbackCancellationToken<'_> {
        CallbackCancellationToken {
            flag: &self.cancelled,
        }
    }
}

/// Token for callback-based cancellation.
#[derive(Clone, Copy)]
pub struct CallbackCancellationToken<'a> {
    flag: &'a AtomicBool,
}

impl Stop for CallbackCancellationToken<'_> {
    fn check(&self) -> Result<(), StopReason> {
        if self.flag.load(Ordering::Acquire) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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

    #[test]
    fn callback_cancellation_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CallbackCancellation<fn()>>();
    }
}

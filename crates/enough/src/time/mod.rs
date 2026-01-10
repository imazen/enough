//! Timeout support for cancellation.
//!
//! This module provides timeout wrappers that add deadline-based cancellation
//! to any [`Stop`] implementation.
//!
//! # Overview
//!
//! - [`WithTimeout`] - Wraps any `Stop` and adds a deadline
//! - [`TimeoutExt`] - Extension trait providing `.with_timeout()` and `.with_deadline()`
//!
//! # Example
//!
//! ```rust
//! use enough::{AtomicStop, Stop, TimeoutExt};
//! use std::time::Duration;
//!
//! let source = AtomicStop::new();
//! let token = source.token().with_timeout(Duration::from_secs(30));
//!
//! // Token will stop if cancelled OR if 30 seconds pass
//! assert!(!token.should_stop());
//! ```
//!
//! # Timeout Tightening
//!
//! Timeouts can only get stricter, never looser. This is safe for composition:
//!
//! ```rust
//! use enough::{AtomicStop, TimeoutExt};
//! use std::time::Duration;
//!
//! let source = AtomicStop::new();
//! let token = source.token()
//!     .with_timeout(Duration::from_secs(60))  // 60 second outer limit
//!     .with_timeout(Duration::from_secs(10)); // 10 second inner limit
//!
//! // Effective timeout is ~10 seconds (the tighter of the two)
//! ```

use std::time::{Duration, Instant};

use crate::{Stop, StopReason};

/// A [`Stop`] wrapper that adds a deadline.
///
/// The wrapped stop will return [`StopReason::TimedOut`] if the deadline
/// passes, or propagate the inner stop's reason if it stops first.
///
/// # Example
///
/// ```rust
/// use enough::{AtomicStop, Stop};
/// use enough::time::WithTimeout;
/// use std::time::Duration;
///
/// let source = AtomicStop::new();
/// let timeout = WithTimeout::new(source.token(), Duration::from_millis(100));
///
/// assert!(!timeout.should_stop());
///
/// std::thread::sleep(Duration::from_millis(150));
/// assert!(timeout.should_stop());
/// ```
#[derive(Debug, Clone)]
pub struct WithTimeout<T> {
    inner: T,
    deadline: Instant,
}

impl<T: Stop> WithTimeout<T> {
    /// Create a new timeout wrapper.
    ///
    /// The deadline is calculated as `Instant::now() + duration`.
    #[inline]
    pub fn new(inner: T, duration: Duration) -> Self {
        Self {
            inner,
            deadline: Instant::now() + duration,
        }
    }

    /// Create a timeout wrapper with an absolute deadline.
    #[inline]
    pub fn with_deadline(inner: T, deadline: Instant) -> Self {
        Self { inner, deadline }
    }

    /// Get the deadline.
    #[inline]
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Get the remaining time until deadline.
    ///
    /// Returns `Duration::ZERO` if the deadline has passed.
    #[inline]
    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// Get a reference to the inner stop.
    #[inline]
    pub fn inner(&self) -> &T {
        &self.inner
    }

    /// Unwrap and return the inner stop.
    #[inline]
    pub fn into_inner(self) -> T {
        self.inner
    }
}

impl<T: Stop> Stop for WithTimeout<T> {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        // Check inner first (may be Cancelled)
        self.inner.check()?;
        // Then check timeout
        if Instant::now() >= self.deadline {
            Err(StopReason::TimedOut)
        } else {
            Ok(())
        }
    }

    #[inline]
    fn should_stop(&self) -> bool {
        self.inner.should_stop() || Instant::now() >= self.deadline
    }
}

/// Extension trait for adding timeouts to any [`Stop`] implementation.
///
/// This trait is automatically implemented for all `Stop` types.
///
/// # Example
///
/// ```rust
/// use enough::{AtomicStop, Stop, TimeoutExt};
/// use std::time::Duration;
///
/// let source = AtomicStop::new();
/// let token = source.token().with_timeout(Duration::from_secs(30));
///
/// assert!(!token.should_stop());
/// ```
pub trait TimeoutExt: Stop + Sized {
    /// Add a timeout to this stop.
    ///
    /// The resulting stop will return [`StopReason::TimedOut`] if the
    /// duration elapses before the operation completes.
    ///
    /// # Timeout Tightening
    ///
    /// If called multiple times, the earliest deadline wins:
    ///
    /// ```rust
    /// use enough::{AtomicStop, TimeoutExt};
    /// use std::time::Duration;
    ///
    /// let source = AtomicStop::new();
    /// let token = source.token()
    ///     .with_timeout(Duration::from_secs(60))
    ///     .with_timeout(Duration::from_secs(10));
    ///
    /// // Effective timeout is ~10 seconds
    /// assert!(token.remaining() < Duration::from_secs(11));
    /// ```
    #[inline]
    fn with_timeout(self, duration: Duration) -> WithTimeout<Self> {
        WithTimeout::new(self, duration)
    }

    /// Add an absolute deadline to this stop.
    ///
    /// If called multiple times, the earliest deadline wins.
    #[inline]
    fn with_deadline(self, deadline: Instant) -> WithTimeout<Self> {
        WithTimeout::with_deadline(self, deadline)
    }
}

impl<T: Stop> TimeoutExt for T {}

impl<T: Stop> WithTimeout<T> {
    /// Add another timeout, taking the tighter of the two deadlines.
    ///
    /// This prevents timeout nesting by updating the deadline in place.
    #[inline]
    pub fn tighten(self, duration: Duration) -> Self {
        let new_deadline = Instant::now() + duration;
        Self {
            inner: self.inner,
            deadline: self.deadline.min(new_deadline),
        }
    }

    /// Add another deadline, taking the earlier of the two.
    ///
    /// This prevents timeout nesting by updating the deadline in place.
    #[inline]
    pub fn tighten_deadline(self, deadline: Instant) -> Self {
        Self {
            inner: self.inner,
            deadline: self.deadline.min(deadline),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AtomicStop;

    #[test]
    fn with_timeout_basic() {
        let source = AtomicStop::new();
        let token = source.token().with_timeout(Duration::from_millis(100));

        assert!(!token.should_stop());
        assert!(token.check().is_ok());

        std::thread::sleep(Duration::from_millis(150));

        assert!(token.should_stop());
        assert_eq!(token.check(), Err(StopReason::TimedOut));
    }

    #[test]
    fn cancel_before_timeout() {
        let source = AtomicStop::new();
        let token = source.token().with_timeout(Duration::from_secs(60));

        source.cancel();

        assert!(token.should_stop());
        assert_eq!(token.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn timeout_tightens() {
        let source = AtomicStop::new();
        let token = source
            .token()
            .with_timeout(Duration::from_secs(60))
            .tighten(Duration::from_secs(1));

        let remaining = token.remaining();
        assert!(remaining < Duration::from_secs(2));
    }

    #[test]
    fn with_deadline_basic() {
        let source = AtomicStop::new();
        let deadline = Instant::now() + Duration::from_millis(100);
        let token = source.token().with_deadline(deadline);

        assert!(!token.should_stop());

        std::thread::sleep(Duration::from_millis(150));

        assert!(token.should_stop());
    }

    #[test]
    fn remaining_accuracy() {
        let source = AtomicStop::new();
        let token = source.token().with_timeout(Duration::from_secs(10));

        let remaining = token.remaining();
        assert!(remaining > Duration::from_secs(9));
        assert!(remaining <= Duration::from_secs(10));
    }

    #[test]
    fn remaining_after_expiry() {
        let source = AtomicStop::new();
        let token = source.token().with_timeout(Duration::from_millis(1));

        std::thread::sleep(Duration::from_millis(10));

        assert_eq!(token.remaining(), Duration::ZERO);
    }

    #[test]
    fn inner_access() {
        let source = AtomicStop::new();
        let token = source.token().with_timeout(Duration::from_secs(10));

        assert!(!token.inner().should_stop());

        source.cancel();

        assert!(token.inner().should_stop());
    }

    #[test]
    fn into_inner() {
        let source = AtomicStop::new();
        let token = source.token().with_timeout(Duration::from_secs(10));

        let inner = token.into_inner();
        assert!(!inner.should_stop());
    }

    #[test]
    fn with_timeout_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<WithTimeout<crate::AtomicToken<'_>>>();
    }
}

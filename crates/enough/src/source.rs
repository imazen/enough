//! Cancellation source - the owner that can trigger cancellation.
//!
//! This module requires the `std` feature.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{Stop, StopReason};

/// Inner state shared between source and tokens.
#[derive(Debug)]
struct Inner {
    cancelled: AtomicBool,
}

/// A cancellation source that can be used to cancel operations.
///
/// Create a source, get tokens from it, and pass those tokens to operations.
/// When you call `cancel()`, all tokens will report cancellation.
///
/// # Example
///
/// ```rust
/// use enough::{CancellationSource, Stop};
///
/// let source = CancellationSource::new();
/// let token = source.token();
///
/// // Pass token to some operation
/// assert!(!token.is_stopped());
///
/// // Cancel when needed
/// source.cancel();
/// assert!(token.is_stopped());
/// ```
#[derive(Debug, Clone)]
pub struct CancellationSource {
    inner: Arc<Inner>,
}

impl CancellationSource {
    /// Create a new cancellation source.
    #[inline]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                cancelled: AtomicBool::new(false),
            }),
        }
    }

    /// Cancel all tokens derived from this source.
    ///
    /// This is idempotent - calling it multiple times has no additional effect.
    #[inline]
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Release);
    }

    /// Check if this source has been cancelled.
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    /// Get a token that can be passed to operations.
    ///
    /// The token is `Clone` and can be freely copied and shared.
    #[inline]
    pub fn token(&self) -> CancellationToken {
        CancellationToken {
            inner: Some(Arc::clone(&self.inner)),
            deadline: None,
        }
    }
}

impl Default for CancellationSource {
    fn default() -> Self {
        Self::new()
    }
}

/// A cancellation token that can be checked for cancellation.
///
/// Tokens are cheap to clone and can be freely shared across threads.
/// They can optionally have a deadline for timeout-based cancellation.
///
/// # Example
///
/// ```rust
/// use enough::{CancellationSource, Stop};
/// use std::time::Duration;
///
/// let source = CancellationSource::new();
/// let token = source.token().with_timeout(Duration::from_secs(30));
///
/// // Check in your operation
/// if token.is_stopped() {
///     // Handle cancellation or timeout
/// }
/// ```
#[derive(Debug, Clone)]
pub struct CancellationToken {
    inner: Option<Arc<Inner>>,
    deadline: Option<Instant>,
}

impl CancellationToken {
    /// Create a token that is never cancelled.
    ///
    /// This is useful as a default when cancellation is not needed.
    /// Equivalent to using `Never`, but as a `CancellationToken` type.
    #[inline]
    pub fn never() -> Self {
        Self {
            inner: None,
            deadline: None,
        }
    }

    /// Add a timeout to this token.
    ///
    /// The timeout is added to the current time to create a deadline.
    /// If the token already has a deadline, the earlier one wins.
    ///
    /// # Example
    ///
    /// ```rust
    /// use enough::CancellationSource;
    /// use std::time::Duration;
    ///
    /// let source = CancellationSource::new();
    /// let token = source.token()
    ///     .with_timeout(Duration::from_secs(30));
    /// ```
    #[inline]
    pub fn with_timeout(self, duration: Duration) -> Self {
        self.with_deadline(Instant::now() + duration)
    }

    /// Add an absolute deadline to this token.
    ///
    /// If the token already has a deadline, the earlier one wins.
    #[inline]
    pub fn with_deadline(self, new_deadline: Instant) -> Self {
        let deadline = match self.deadline {
            Some(existing) => Some(existing.min(new_deadline)),
            None => Some(new_deadline),
        };
        Self { deadline, ..self }
    }

    /// Get the deadline, if any.
    #[inline]
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Get the remaining time until deadline, if any.
    ///
    /// Returns `None` if there is no deadline.
    /// Returns `Some(Duration::ZERO)` if the deadline has passed.
    #[inline]
    pub fn remaining(&self) -> Option<Duration> {
        self.deadline
            .map(|d| d.saturating_duration_since(Instant::now()))
    }

    /// Check if the deadline has passed.
    #[inline]
    fn is_timed_out(&self) -> bool {
        self.deadline.map(|d| Instant::now() >= d).unwrap_or(false)
    }

    /// Check if the source was cancelled.
    #[inline]
    fn is_source_cancelled(&self) -> bool {
        self.inner
            .as_ref()
            .map(|i| i.cancelled.load(Ordering::Acquire))
            .unwrap_or(false)
    }
}

impl Stop for CancellationToken {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        // Check cancellation first (cheaper)
        if self.is_source_cancelled() {
            return Err(StopReason::Cancelled);
        }
        // Then check timeout
        if self.is_timed_out() {
            return Err(StopReason::TimedOut);
        }
        Ok(())
    }

    #[inline]
    fn is_stopped(&self) -> bool {
        self.is_source_cancelled() || self.is_timed_out()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_basic_usage() {
        let source = CancellationSource::new();
        assert!(!source.is_cancelled());

        let token = source.token();
        assert!(!token.is_stopped());

        source.cancel();

        assert!(source.is_cancelled());
        assert!(token.is_stopped());
    }

    #[test]
    fn token_is_clone() {
        let source = CancellationSource::new();
        let t1 = source.token();
        let t2 = t1.clone();
        let t3 = t1.clone();

        source.cancel();

        assert!(t1.is_stopped());
        assert!(t2.is_stopped());
        assert!(t3.is_stopped());
    }

    #[test]
    fn never_token() {
        let token = CancellationToken::never();
        assert!(!token.is_stopped());
        assert!(token.check().is_ok());
    }

    #[test]
    fn timeout_works() {
        let source = CancellationSource::new();
        let token = source.token().with_timeout(Duration::from_millis(1));

        std::thread::sleep(Duration::from_millis(10));

        assert!(token.is_stopped());
        assert_eq!(token.check(), Err(StopReason::TimedOut));
    }

    #[test]
    fn cancel_before_timeout() {
        let source = CancellationSource::new();
        let token = source.token().with_timeout(Duration::from_secs(60));

        source.cancel();

        // Should be Cancelled, not TimedOut
        assert_eq!(token.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn timeout_tightens() {
        let source = CancellationSource::new();
        let token = source
            .token()
            .with_timeout(Duration::from_secs(60))
            .with_timeout(Duration::from_secs(1));

        let remaining = token.remaining().unwrap();
        assert!(remaining < Duration::from_secs(2));
    }

    #[test]
    fn source_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CancellationSource>();
        assert_send_sync::<CancellationToken>();
    }
}

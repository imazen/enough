//! Cancellation token - lightweight, Copy check handle.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use enough::{Stop, StopReason};

use crate::CancellationSource;

/// A lightweight, `Copy` token for checking cancellation.
///
/// Tokens are created from a [`CancellationSource`] and can be freely
/// copied and passed around. They contain just a pointer to the source's
/// atomic flag, plus an optional deadline.
///
/// # Safety
///
/// The token is valid as long as the source it was created from exists.
/// Using a token after its source is dropped is undefined behavior.
///
/// # Example
///
/// ```rust
/// use enough_std::{CancellationSource, CancellationToken};
/// use enough::Stop;
/// use std::time::Duration;
///
/// let source = CancellationSource::new();
/// let token = source.token()
///     .with_timeout(Duration::from_secs(30));
///
/// // Check in a loop
/// for i in 0..1000 {
///     if i % 100 == 0 {
///         if let Err(reason) = token.check() {
///             println!("Stopped: {}", reason);
///             break;
///         }
///     }
///     // do work...
/// }
/// ```
#[derive(Clone, Copy)]
pub struct CancellationToken {
    /// Pointer to the source's atomic flag. Null means never cancelled.
    pub(crate) flag: *const AtomicBool,
    /// Optional deadline. None means no timeout.
    deadline: Option<Instant>,
}

// SAFETY: AtomicBool is Sync, and we only read from it.
// The pointer is valid as long as the source exists, which is the user's
// responsibility (documented in safety section).
unsafe impl Send for CancellationToken {}
unsafe impl Sync for CancellationToken {}

impl CancellationToken {
    /// Create a token that never cancels.
    ///
    /// This is equivalent to using [`enough::Never`] but in token form,
    /// useful when you need a concrete type rather than a generic.
    #[inline]
    pub const fn never() -> Self {
        Self {
            flag: std::ptr::null(),
            deadline: None,
        }
    }

    /// Create a token from a source.
    #[inline]
    pub(crate) fn from_source(source: &CancellationSource) -> Self {
        Self {
            flag: source.flag_ptr(),
            deadline: None,
        }
    }

    /// Create a token from a raw flag pointer.
    ///
    /// # Safety
    ///
    /// The pointer must be valid for the lifetime of all uses of this token,
    /// or null (which creates a never-cancelled token).
    #[inline]
    pub const unsafe fn from_raw(flag: *const AtomicBool) -> Self {
        Self {
            flag,
            deadline: None,
        }
    }

    /// Add a timeout to this token.
    ///
    /// The timeout is combined with any existing deadline - the *sooner*
    /// deadline wins. This ensures timeouts only tighten, never loosen.
    ///
    /// # Example
    ///
    /// ```rust
    /// use enough_std::CancellationSource;
    /// use std::time::Duration;
    ///
    /// let source = CancellationSource::new();
    ///
    /// // Parent has 60s timeout
    /// let parent = source.token().with_timeout(Duration::from_secs(60));
    ///
    /// // Child wants 10s - gets min(remaining_parent, 10s)
    /// let child = parent.with_timeout(Duration::from_secs(10));
    /// ```
    #[inline]
    pub fn with_timeout(self, duration: Duration) -> Self {
        self.with_deadline(Instant::now() + duration)
    }

    /// Add an absolute deadline to this token.
    ///
    /// The deadline is combined with any existing deadline - the *sooner*
    /// deadline wins.
    #[inline]
    pub fn with_deadline(self, new_deadline: Instant) -> Self {
        let deadline = match self.deadline {
            Some(existing) => Some(existing.min(new_deadline)),
            None => Some(new_deadline),
        };
        Self { deadline, ..self }
    }

    /// Get the current deadline, if any.
    #[inline]
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Get the remaining time until deadline, if any.
    #[inline]
    pub fn remaining(&self) -> Option<Duration> {
        self.deadline
            .map(|d| d.saturating_duration_since(Instant::now()))
    }

    /// Check if the flag is set (ignoring deadline).
    #[inline]
    fn is_flag_set(&self) -> bool {
        if self.flag.is_null() {
            false
        } else {
            // SAFETY: Caller guarantees flag is valid while token is in use
            unsafe { (*self.flag).load(Ordering::Acquire) }
        }
    }

    /// Check if the deadline has passed.
    #[inline]
    fn is_deadline_passed(&self) -> bool {
        self.deadline.map(|d| Instant::now() >= d).unwrap_or(false)
    }
}

impl Stop for CancellationToken {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        // Check flag first (cheaper than Instant::now())
        if self.is_flag_set() {
            return Err(StopReason::Cancelled);
        }
        // Then check deadline
        if self.is_deadline_passed() {
            return Err(StopReason::TimedOut);
        }
        Ok(())
    }

    #[inline]
    fn is_stopped(&self) -> bool {
        self.is_flag_set() || self.is_deadline_passed()
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::never()
    }
}

impl std::fmt::Debug for CancellationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancellationToken")
            .field("has_flag", &!self.flag.is_null())
            .field("deadline", &self.deadline)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_token_never_stops() {
        let token = CancellationToken::never();
        assert!(!token.is_stopped());
        assert!(token.check().is_ok());
    }

    #[test]
    fn token_is_copy() {
        let source = CancellationSource::new();
        let token = source.token();
        let copy = token; // Copy
        let _ = token; // Original still valid
        let _ = copy;
    }

    #[test]
    fn token_with_timeout() {
        let source = CancellationSource::new();
        let token = source.token().with_timeout(Duration::from_millis(10));

        assert!(!token.is_stopped());
        std::thread::sleep(Duration::from_millis(20));
        assert!(token.is_stopped());
        assert_eq!(token.check(), Err(StopReason::TimedOut));
    }

    #[test]
    fn token_timeout_tightens() {
        let source = CancellationSource::new();
        let token = source
            .token()
            .with_timeout(Duration::from_secs(60)) // 60s
            .with_timeout(Duration::from_secs(10)); // Should be ~10s, not 70s

        let remaining = token.remaining().unwrap();
        assert!(remaining < Duration::from_secs(15));
        assert!(remaining > Duration::from_secs(5));
    }

    #[test]
    fn token_cancel_before_timeout() {
        let source = CancellationSource::new();
        let token = source.token().with_timeout(Duration::from_secs(60));

        assert!(!token.is_stopped());
        source.cancel();
        assert!(token.is_stopped());
        assert_eq!(token.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn token_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CancellationToken>();
    }

    #[test]
    fn token_default() {
        let token: CancellationToken = Default::default();
        assert!(!token.is_stopped());
    }

    #[test]
    fn token_debug() {
        let source = CancellationSource::new();
        let token = source.token();
        let debug = format!("{:?}", token);
        assert!(debug.contains("CancellationToken"));
    }

    #[test]
    fn token_remaining() {
        let source = CancellationSource::new();
        let token = source.token().with_timeout(Duration::from_secs(10));

        let remaining = token.remaining();
        assert!(remaining.is_some());
        assert!(remaining.unwrap() <= Duration::from_secs(10));
        assert!(remaining.unwrap() > Duration::from_secs(9));
    }

    #[test]
    fn never_token_no_remaining() {
        let token = CancellationToken::never();
        assert!(token.remaining().is_none());
    }
}

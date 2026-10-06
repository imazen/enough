//! Stop reason type.

use core::fmt;

/// Why an operation was stopped.
///
/// This is returned from [`Stop::check()`](crate::Stop::check) when the
/// operation should stop.
///
/// # Error Integration
///
/// Implement `From<StopReason>` for your error type to use `?` naturally:
///
/// ```rust
/// use enough::StopReason;
///
/// #[derive(Debug)]
/// enum MyError {
///     Stopped(StopReason),
///     Io(std::io::Error),
/// }
///
/// impl From<StopReason> for MyError {
///     fn from(r: StopReason) -> Self { MyError::Stopped(r) }
/// }
/// ```
///
/// Implement [`AsStopReason`] too, so code holding your error can tell a
/// cancellation or timeout from a failure without knowing your error type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StopReason {
    /// Operation was explicitly cancelled.
    ///
    /// This typically means someone called `cancel()` on a cancellation source,
    /// or a parent operation was cancelled.
    Cancelled,

    /// Operation exceeded its deadline.
    ///
    /// This means a timeout was set and the deadline passed before the
    /// operation completed.
    TimedOut,
}

impl StopReason {
    /// Returns `true` if this is a transient condition that might succeed on retry.
    ///
    /// Currently only `TimedOut` is considered transient, as the operation might
    /// succeed with a longer timeout or under less load.
    ///
    /// `Cancelled` is not transient - it represents an explicit decision to stop.
    #[inline]
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::TimedOut)
    }

    /// Returns `true` if this was an explicit cancellation.
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    /// Returns `true` if this was a timeout.
    #[inline]
    pub fn is_timed_out(&self) -> bool {
        matches!(self, Self::TimedOut)
    }
}

/// The [`StopReason`] an error represents, if it represents one.
///
/// An operation that was cancelled or timed out usually returns an error
/// that wraps the [`StopReason`] it got from [`Stop::check`](crate::Stop::check).
/// Callers use this trait to tell that apart from a real failure without
/// knowing the error type: to skip a retry, log quietly, or record a progress
/// phase as cancelled. [`StopReason`] implements it; an error type with a
/// variant that wraps a `StopReason` implements it next to its
/// `From<StopReason>`:
///
/// ```rust
/// use enough::{AsStopReason, StopReason};
///
/// enum MyError {
///     Corrupt,
///     Stopped(StopReason),
/// }
/// impl From<StopReason> for MyError {
///     fn from(reason: StopReason) -> Self {
///         Self::Stopped(reason)
///     }
/// }
/// impl AsStopReason for MyError {
///     fn as_stop_reason(&self) -> Option<StopReason> {
///         match self {
///             Self::Stopped(reason) => Some(*reason),
///             _ => None,
///         }
///     }
/// }
/// assert_eq!(
///     MyError::Stopped(StopReason::TimedOut).as_stop_reason(),
///     Some(StopReason::TimedOut)
/// );
/// assert!(MyError::Corrupt.as_stop_reason().is_none());
/// ```
#[diagnostic::on_unimplemented(
    message = "`{Self}` doesn't say whether it represents a cancellation or timeout",
    label = "needs `enough::AsStopReason`",
    note = "implement `enough::AsStopReason for {Self}`, returning `Some(reason)` for the variant that wraps a `StopReason`",
    note = "for an error type you cannot implement it for, map it to a `StopReason` where it is handled"
)]
pub trait AsStopReason {
    /// The [`StopReason`] this error represents, or `None` for any other
    /// failure.
    fn as_stop_reason(&self) -> Option<StopReason>;
}

impl AsStopReason for StopReason {
    #[inline]
    fn as_stop_reason(&self) -> Option<StopReason> {
        Some(*self)
    }
}

impl fmt::Display for StopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(f, "operation cancelled"),
            Self::TimedOut => write!(f, "operation timed out"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_reason_display() {
        extern crate alloc;
        use alloc::format;
        assert_eq!(format!("{}", StopReason::Cancelled), "operation cancelled");
        assert_eq!(format!("{}", StopReason::TimedOut), "operation timed out");
    }

    #[test]
    fn stop_reason_equality() {
        assert_eq!(StopReason::Cancelled, StopReason::Cancelled);
        assert_eq!(StopReason::TimedOut, StopReason::TimedOut);
        assert_ne!(StopReason::Cancelled, StopReason::TimedOut);
    }

    #[test]
    fn stop_reason_is_transient() {
        assert!(!StopReason::Cancelled.is_transient());
        assert!(StopReason::TimedOut.is_transient());
    }

    #[test]
    fn stop_reason_copy() {
        let a = StopReason::Cancelled;
        let b = a; // Copy
        assert_eq!(a, b);
    }

    #[test]
    fn stop_reason_hash() {
        use core::hash::{Hash, Hasher};

        struct DummyHasher(u64);
        impl Hasher for DummyHasher {
            fn finish(&self) -> u64 {
                self.0
            }
            fn write(&mut self, bytes: &[u8]) {
                for &b in bytes {
                    self.0 = self.0.wrapping_add(b as u64);
                }
            }
        }

        let mut h1 = DummyHasher(0);
        let mut h2 = DummyHasher(0);
        StopReason::Cancelled.hash(&mut h1);
        StopReason::Cancelled.hash(&mut h2);
        assert_eq!(h1.finish(), h2.finish());
    }
}

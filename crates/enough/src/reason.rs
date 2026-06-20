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

impl fmt::Display for StopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(f, "operation cancelled"),
            Self::TimedOut => write!(f, "operation timed out"),
        }
    }
}

// `core::error::Error` (stable since Rust 1.81; this crate's MSRV is 1.85) is
// available in `no_std` with no feature flag, so implementing it costs nothing
// on the `no_std` feature surface. `StopReason` is a leaf — it has no inner
// cause, so `source()` keeps the default `None`. The value of the impl is that
// `StopReason` can now appear as a *findable cause* in another error type's
// `source()` chain: a consumer that wraps it (`impl From<StopReason> for
// MyError`) can expose it via `#[source]`/`#[from]`, and a generic caller can
// recover it with a downcast walk (e.g. zencodec's `CodecErrorExt::cancelled`)
// without naming the concrete error enum. (Re-added after 0.4.0's removal,
// which predated the classification use case and targeted `std::error::Error`.)
impl core::error::Error for StopReason {}

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
    fn stop_reason_is_error_and_downcastable() {
        // The point of the re-added impl: StopReason can be a findable cause in
        // another error's source() chain and be recovered by a downcast walk.
        let r = StopReason::Cancelled;
        let dyn_err: &(dyn core::error::Error + 'static) = &r;
        assert!(dyn_err.source().is_none(), "StopReason is a leaf cause");
        assert_eq!(
            dyn_err.downcast_ref::<StopReason>(),
            Some(&StopReason::Cancelled)
        );
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

//! Function-based cancellation.
//!
//! This module provides a [`Stop`] implementation that wraps a closure.
//! Works in `no_std` environments.
//!
//! # Example
//!
//! ```rust
//! use almost_enough::{FnStop, Stop};
//! use core::sync::atomic::{AtomicBool, Ordering};
//!
//! static CANCELLED: AtomicBool = AtomicBool::new(false);
//!
//! let stop = FnStop::new(|| CANCELLED.load(Ordering::Relaxed));
//!
//! assert!(!stop.should_stop());
//!
//! CANCELLED.store(true, Ordering::Relaxed);
//! assert!(stop.should_stop());
//! ```
//!
//! # Integration with Other Systems
//!
//! `FnStop` is useful for bridging to external cancellation mechanisms:
//!
//! ```rust,ignore
//! use almost_enough::{FnStop, Stop};
//!
//! // Bridge to tokio CancellationToken
//! let tokio_token = tokio_util::sync::CancellationToken::new();
//! let stop = FnStop::new({
//!     let t = tokio_token.clone();
//!     move || t.is_cancelled()
//! });
//!
//! // Bridge to crossbeam channel
//! let (tx, rx) = crossbeam_channel::bounded::<()>(1);
//! let stop = FnStop::new(move || rx.try_recv().is_ok());
//! ```

use crate::{Stop, StopReason};

/// A [`Stop`] implementation backed by a closure.
///
/// The closure should return `true` when the operation should stop.
///
/// # Example
///
/// ```rust
/// use almost_enough::{FnStop, Stop};
/// use core::sync::atomic::{AtomicBool, Ordering};
///
/// let flag = AtomicBool::new(false);
/// let stop = FnStop::new(|| flag.load(Ordering::Relaxed));
///
/// assert!(!stop.should_stop());
///
/// flag.store(true, Ordering::Relaxed);
/// assert!(stop.should_stop());
/// ```
pub struct FnStop<F> {
    f: F,
}

impl<F> FnStop<F>
where
    F: Fn() -> bool + Send + Sync,
{
    /// Create a new function-based stop.
    ///
    /// The function should return `true` when the operation should stop.
    #[inline]
    pub fn new(f: F) -> Self {
        Self { f }
    }
}

impl<F> Stop for FnStop<F>
where
    F: Fn() -> bool + Send + Sync,
{
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        if (self.f)() {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }

    #[inline]
    fn should_stop(&self) -> bool {
        (self.f)()
    }
}

impl<F: Clone> Clone for FnStop<F> {
    fn clone(&self) -> Self {
        Self { f: self.f.clone() }
    }
}

impl<F: Copy> Copy for FnStop<F> {}

impl<F> core::fmt::Debug for FnStop<F> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FnStop").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn fn_stop_basic() {
        let flag = AtomicBool::new(false);
        let stop = FnStop::new(|| flag.load(Ordering::Relaxed));

        assert!(!stop.should_stop());
        assert!(stop.check().is_ok());

        flag.store(true, Ordering::Relaxed);

        assert!(stop.should_stop());
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn fn_stop_with_static() {
        static FLAG: AtomicBool = AtomicBool::new(false);

        let stop = FnStop::new(|| FLAG.load(Ordering::Relaxed));
        assert!(!stop.should_stop());

        FLAG.store(true, Ordering::Relaxed);
        assert!(stop.should_stop());

        // Reset for other tests
        FLAG.store(false, Ordering::Relaxed);
    }

    #[test]
    fn fn_stop_always_true() {
        let stop = FnStop::new(|| true);
        assert!(stop.should_stop());
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn fn_stop_always_false() {
        let stop = FnStop::new(|| false);
        assert!(!stop.should_stop());
        assert!(stop.check().is_ok());
    }

    #[test]
    fn fn_stop_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FnStop<fn() -> bool>>();
    }

    #[test]
    fn fn_stop_copy() {
        // Note: closures that borrow aren't Copy, but fn pointers are
        let stop: FnStop<fn() -> bool> = FnStop::new(|| false);
        let stop2 = stop; // Copy, not Clone
        assert!(!stop.should_stop()); // Original still usable
        assert!(!stop2.should_stop());
    }
}

#[cfg(all(test, feature = "alloc"))]
mod alloc_tests {
    use super::*;

    #[test]
    fn fn_stop_debug() {
        extern crate alloc;
        let stop = FnStop::new(|| false);
        let debug = alloc::format!("{:?}", stop);
        assert!(debug.contains("FnStop"));
    }
}

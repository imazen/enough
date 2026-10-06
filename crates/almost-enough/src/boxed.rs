//! Boxed dynamic dispatch for Stop.
//!
//! This module provides [`BoxedStop`], a heap-allocated wrapper that enables
//! dynamic dispatch without monomorphization bloat.
//!
//! # Deprecated
//!
//! Use [`StopToken`](crate::StopToken): it checks through the same fast
//! paths and is `Clone`. `BoxedStop` wraps one; it adds nothing but the
//! absence of `Clone`.
//!
//! # Alternatives
//!
//! For borrowed dynamic dispatch with zero allocation, use `&dyn Stop`:
//!
//! ```rust
//! use almost_enough::{StopSource, Stop};
//!
//! fn process(stop: &dyn Stop) {
//!     if stop.should_stop() {
//!         return;
//!     }
//!     // ...
//! }
//!
//! let source = StopSource::new();
//! process(&source);
//! ```

#![allow(deprecated)] // This module defines and tests the deprecated type.

use crate::{Stop, StopReason, StopToken};

/// A type-erased [`Stop`] with unique ownership. Deprecated: use
/// [`StopToken`], which checks the same way and is `Clone`.
///
/// `BoxedStop` wraps a `StopToken`: no-op stops (like `Unstoppable`) are stored
/// as nothing and never dispatched, a `Stopper` is checked as a direct atomic
/// load without a vtable, a `SyncStopper` keeps its own `Arc` behind the
/// vtable, and wrapping a `StopToken` or another `BoxedStop` reuses it instead
/// of nesting. Anything else is allocated and checked through a vtable.
///
/// # Example
///
/// ```rust
/// use almost_enough::{BoxedStop, StopSource, Stopper, Unstoppable, Stop};
///
/// fn process(stop: BoxedStop) {
///     for i in 0..1000 {
///         if i % 100 == 0 && stop.should_stop() {
///             return;
///         }
///         // process...
///     }
/// }
///
/// // Works with any Stop implementation
/// process(BoxedStop::new(Unstoppable));
/// process(BoxedStop::new(StopSource::new()));
/// process(BoxedStop::new(Stopper::new()));
/// ```
#[deprecated(
    since = "0.4.5",
    note = "use `StopToken`, which checks the same way and is `Clone`"
)]
pub struct BoxedStop(pub(crate) StopToken);

impl BoxedStop {
    /// Create a new boxed stop from any [`Stop`] implementation.
    ///
    /// No-op stops (where `may_stop()` returns false) are not allocated —
    /// `check()` will short-circuit to `Ok(())`. See [`StopToken::new`] for
    /// the other cases that don't allocate.
    #[inline]
    pub fn new<T: Stop + 'static>(stop: T) -> Self {
        Self(StopToken::new(stop))
    }
}

impl Stop for BoxedStop {
    #[inline(always)]
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.0.check()
    }

    #[inline(always)]
    #[track_caller]
    fn should_stop(&self) -> bool {
        self.0.should_stop()
    }

    #[inline(always)]
    fn may_stop(&self) -> bool {
        self.0.may_stop()
    }
}

impl core::fmt::Debug for BoxedStop {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("BoxedStop").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StopSource, Stopper, Unstoppable};

    #[test]
    fn boxed_stop_from_unstoppable() {
        let stop = BoxedStop::new(Unstoppable);
        assert!(!stop.should_stop());
        assert!(stop.check().is_ok());
        assert!(!stop.may_stop());
    }

    #[test]
    fn boxed_stop_from_stopper() {
        let stopper = Stopper::new();
        let stop = BoxedStop::new(stopper.clone());

        assert!(!stop.should_stop());

        stopper.cancel();

        assert!(stop.should_stop());
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn boxed_stop_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BoxedStop>();
    }

    #[test]
    fn boxed_stop_debug() {
        let stop = BoxedStop::new(Unstoppable);
        let debug = alloc::format!("{:?}", stop);
        assert!(debug.contains("BoxedStop"));
    }

    #[test]
    fn boxed_stop_avoids_monomorphization() {
        fn process(stop: BoxedStop) -> bool {
            stop.should_stop()
        }

        assert!(!process(BoxedStop::new(Unstoppable)));
        assert!(!process(BoxedStop::new(StopSource::new())));
        assert!(!process(BoxedStop::new(Stopper::new())));
    }

    #[test]
    fn may_stop_delegates_through_boxed() {
        assert!(!BoxedStop::new(Unstoppable).may_stop());
        assert!(BoxedStop::new(Stopper::new()).may_stop());
    }

    #[test]
    fn unstoppable_no_allocation() {
        // Unstoppable wraps to None — no heap allocation
        let stop = BoxedStop::new(Unstoppable);
        assert!(!stop.may_stop());
        assert!(stop.check().is_ok());
    }
}

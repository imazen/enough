//! Boxed dynamic dispatch for Stop.
//!
//! This module provides [`BoxedStop`], a heap-allocated wrapper that enables
//! dynamic dispatch without monomorphization bloat.
//!
//! # When to Use
//!
//! **Prefer [`StopToken`](crate::StopToken)** which is `Clone` (via `Arc`).
//! `BoxedStop` is retained for cases where unique ownership is required.
//!
//! Generic functions like `fn process(stop: impl Stop)` are monomorphized
//! for each concrete type, increasing binary size. `BoxedStop` provides a
//! single concrete type for dynamic dispatch:
//!
//! ```rust
//! use almost_enough::{BoxedStop, Stop};
//!
//! // Single implementation - no monomorphization bloat
//! fn process_boxed(stop: BoxedStop) {
//!     // ...
//! }
//! ```
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

use alloc::boxed::Box;

use crate::{Stop, StopReason};

/// A heap-allocated [`Stop`] implementation.
///
/// **Prefer [`StopToken`](crate::StopToken)** which is `Clone` (via `Arc`) and
/// supports indirection collapsing. `BoxedStop` is retained for cases where
/// unique ownership is required.
///
/// No-op stops (like `Unstoppable`) are optimized away at construction —
/// `check()` short-circuits without any vtable dispatch.
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
pub struct BoxedStop(Option<Box<dyn Stop + Send + Sync>>);

impl BoxedStop {
    /// Create a new boxed stop from any [`Stop`] implementation.
    ///
    /// No-op stops (where `may_stop()` returns false) are not allocated —
    /// `check()` will short-circuit to `Ok(())`.
    #[inline]
    pub fn new<T: Stop + 'static>(stop: T) -> Self {
        if !stop.may_stop() {
            return Self(None);
        }
        Self(Some(Box::new(stop)))
    }
}

impl Stop for BoxedStop {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        match &self.0 {
            Some(inner) => inner.check(),
            None => Ok(()),
        }
    }

    #[inline]
    fn should_stop(&self) -> bool {
        match &self.0 {
            Some(inner) => inner.should_stop(),
            None => false,
        }
    }

    #[inline]
    fn may_stop(&self) -> bool {
        self.0.is_some()
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

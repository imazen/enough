//! Boxed dynamic dispatch for Stop.
//!
//! This module provides [`BoxedStop`], a heap-allocated wrapper that enables
//! dynamic dispatch without monomorphization bloat.
//!
//! # When to Use
//!
//! Generic functions like `fn process(stop: impl Stop)` are monomorphized
//! for each concrete type, increasing binary size. `BoxedStop` provides a
//! single concrete type for dynamic dispatch:
//!
//! ```rust
//! use almost_enough::{BoxedStop, Stop};
//!
//! // Monomorphized for each Stop type - increases binary size
//! fn process_generic(stop: impl Stop) {
//!     // ...
//! }
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
/// This type provides dynamic dispatch for `Stop`, avoiding monomorphization
/// bloat when you don't need the performance of generics.
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
pub struct BoxedStop(Box<dyn Stop + Send + Sync>);

impl BoxedStop {
    /// Create a new boxed stop from any [`Stop`] implementation.
    #[inline]
    pub fn new<T: Stop + 'static>(stop: T) -> Self {
        Self(Box::new(stop))
    }

    /// Returns the effective inner stop if it may stop, collapsing indirection.
    ///
    /// The returned `&dyn Stop` points directly to the concrete type inside
    /// the box, bypassing the `BoxedStop` wrapper. In a hot loop, subsequent
    /// `check()` calls go through one vtable dispatch instead of two.
    ///
    /// Returns `None` if the inner stop is a no-op (e.g., `Unstoppable`).
    ///
    /// # Example
    ///
    /// ```rust
    /// use almost_enough::{BoxedStop, Stopper, Unstoppable, Stop, StopReason};
    ///
    /// fn hot_loop(stop: &BoxedStop) -> Result<(), StopReason> {
    ///     let stop = stop.active_stop(); // Option<&dyn Stop>, collapsed
    ///     for i in 0..1000 {
    ///         stop.check()?;
    ///     }
    ///     Ok(())
    /// }
    ///
    /// // Unstoppable: returns None, check() is always Ok(())
    /// assert!(hot_loop(&BoxedStop::new(Unstoppable)).is_ok());
    ///
    /// // Stopper: returns Some(&Stopper), one vtable dispatch per check()
    /// assert!(hot_loop(&BoxedStop::new(Stopper::new())).is_ok());
    /// ```
    #[inline]
    pub fn active_stop(&self) -> Option<&dyn Stop> {
        let inner: &dyn Stop = &*self.0;
        if inner.may_stop() { Some(inner) } else { None }
    }
}

impl Stop for BoxedStop {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        self.0.check()
    }

    #[inline]
    fn should_stop(&self) -> bool {
        self.0.should_stop()
    }

    #[inline]
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
        // This function has a single concrete implementation
        fn process(stop: BoxedStop) -> bool {
            stop.should_stop()
        }

        // All these use the same process function
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
    fn active_stop_collapses_unstoppable() {
        let stop = BoxedStop::new(Unstoppable);
        assert!(stop.active_stop().is_none());
    }

    #[test]
    fn active_stop_collapses_nested() {
        let inner = BoxedStop::new(Unstoppable);
        let outer = BoxedStop::new(inner);
        assert!(outer.active_stop().is_none());
    }

    #[test]
    fn active_stop_returns_inner_for_stopper() {
        let stopper = Stopper::new();
        let stop = BoxedStop::new(stopper.clone());

        let active = stop.active_stop();
        assert!(active.is_some());
        assert!(active.unwrap().check().is_ok());

        stopper.cancel();
        assert!(active.unwrap().should_stop());
    }

    #[test]
    fn active_stop_hot_loop_pattern() {
        let stop = BoxedStop::new(Unstoppable);
        let active = stop.active_stop();
        for _ in 0..1000 {
            assert!(active.check().is_ok());
        }
    }
}

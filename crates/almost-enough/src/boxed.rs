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
}

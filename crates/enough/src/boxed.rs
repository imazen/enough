//! Boxed dynamic dispatch for Stop.
//!
//! This module provides [`BoxStop`], a heap-allocated wrapper that enables
//! dynamic dispatch without monomorphization bloat.
//!
//! # When to Use
//!
//! Generic functions like `fn process(stop: impl Stop)` are monomorphized
//! for each concrete type, increasing binary size. `BoxStop` provides a
//! single concrete type for dynamic dispatch:
//!
//! ```rust
//! use enough::{BoxStop, Stop};
//!
//! // Monomorphized for each Stop type - increases binary size
//! fn process_generic(stop: impl Stop) {
//!     // ...
//! }
//!
//! // Single implementation - no monomorphization bloat
//! fn process_boxed(stop: BoxStop) {
//!     // ...
//! }
//! ```
//!
//! # Alternatives
//!
//! For borrowed dynamic dispatch with zero allocation, use `&dyn Stop`:
//!
//! ```rust
//! use enough::{AtomicStop, Stop};
//!
//! fn process(stop: &dyn Stop) {
//!     if stop.should_stop() {
//!         return;
//!     }
//!     // ...
//! }
//!
//! let source = AtomicStop::new();
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
/// use enough::{BoxStop, AtomicStop, ArcStop, Never, Stop};
///
/// fn process(stop: BoxStop) {
///     for i in 0..1000 {
///         if i % 100 == 0 && stop.should_stop() {
///             return;
///         }
///         // process...
///     }
/// }
///
/// // Works with any Stop implementation
/// process(BoxStop::new(Never));
/// process(BoxStop::new(AtomicStop::new()));
/// process(BoxStop::new(ArcStop::new()));
/// ```
pub struct BoxStop(Box<dyn Stop + Send + Sync>);

impl BoxStop {
    /// Create a new boxed stop from any [`Stop`] implementation.
    #[inline]
    pub fn new<T: Stop + 'static>(stop: T) -> Self {
        Self(Box::new(stop))
    }
}

impl Stop for BoxStop {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        self.0.check()
    }

    #[inline]
    fn should_stop(&self) -> bool {
        self.0.should_stop()
    }
}

impl core::fmt::Debug for BoxStop {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("BoxStop").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ArcStop, AtomicStop, Never};

    #[test]
    fn boxstop_from_never() {
        let stop = BoxStop::new(Never);
        assert!(!stop.should_stop());
        assert!(stop.check().is_ok());
    }

    #[test]
    fn boxstop_from_arc() {
        let source = ArcStop::new();
        let stop = BoxStop::new(source.token());

        assert!(!stop.should_stop());

        source.cancel();

        assert!(stop.should_stop());
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn boxstop_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BoxStop>();
    }

    #[test]
    fn boxstop_debug() {
        let stop = BoxStop::new(Never);
        let debug = alloc::format!("{:?}", stop);
        assert!(debug.contains("BoxStop"));
    }

    #[test]
    fn boxstop_avoids_monomorphization() {
        // This function has a single concrete implementation
        fn process(stop: BoxStop) -> bool {
            stop.should_stop()
        }

        // All these use the same process function
        assert!(!process(BoxStop::new(Never)));
        assert!(!process(BoxStop::new(AtomicStop::new())));
        assert!(!process(BoxStop::new(ArcStop::new())));
    }
}

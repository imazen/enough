//! # almost-enough
//!
//! Batteries-included ergonomic extensions for the [`enough`] cooperative cancellation crate.
//!
//! This crate provides extension traits and helpers that make working with stop tokens
//! more ergonomic. It re-exports everything from `enough` for convenience.
//!
//! ## StopExt Extension Trait
//!
//! The [`StopExt`] trait adds combinator methods to any [`Stop`] implementation:
//!
//! ```rust
//! use almost_enough::{StopSource, Stop, StopExt};
//!
//! let timeout = StopSource::new();
//! let cancel = StopSource::new();
//!
//! // Combine: stop if either stops
//! let combined = timeout.as_ref().or(cancel.as_ref());
//! assert!(!combined.should_stop());
//!
//! cancel.cancel();
//! assert!(combined.should_stop());
//! ```
//!
//! ## Type Erasure with `into_boxed()`
//!
//! Prevent monomorphization explosion at API boundaries:
//!
//! ```rust
//! # #[cfg(feature = "alloc")]
//! # fn main() {
//! use almost_enough::{Stopper, BoxedStop, Stop, StopExt};
//!
//! fn outer(stop: impl Stop + 'static) {
//!     // Erase the concrete type to avoid monomorphizing inner()
//!     inner(stop.into_boxed());
//! }
//!
//! fn inner(stop: BoxedStop) {
//!     // Only one version of this function exists
//!     while !stop.should_stop() {
//!         break;
//!     }
//! }
//!
//! let stop = Stopper::new();
//! outer(stop);
//! # }
//! # #[cfg(not(feature = "alloc"))]
//! # fn main() {}
//! ```
//!
//! ## Hierarchical Cancellation with `.child()`
//!
//! Create child stops that inherit cancellation from their parent:
//!
//! ```rust
//! # #[cfg(feature = "alloc")]
//! # fn main() {
//! use almost_enough::{Stopper, Stop, StopExt};
//!
//! let parent = Stopper::new();
//! let child = parent.child();
//!
//! // Child cancellation doesn't affect parent
//! child.cancel();
//! assert!(!parent.should_stop());
//!
//! // But parent cancellation propagates to children
//! let child2 = parent.child();
//! parent.cancel();
//! assert!(child2.should_stop());
//! # }
//! # #[cfg(not(feature = "alloc"))]
//! # fn main() {}
//! ```
//!
//! ## Stop Guards (RAII Cancellation)
//!
//! Automatically stop on scope exit unless explicitly disarmed:
//!
//! ```rust
//! # #[cfg(feature = "alloc")]
//! # fn main() {
//! use almost_enough::{Stopper, StopDropRoll};
//!
//! fn do_work(source: &Stopper) -> Result<(), &'static str> {
//!     let guard = source.stop_on_drop();
//!
//!     // If we return early or panic, source is stopped
//!     risky_operation()?;
//!
//!     // Success! Don't stop.
//!     guard.disarm();
//!     Ok(())
//! }
//!
//! fn risky_operation() -> Result<(), &'static str> {
//!     Ok(())
//! }
//!
//! let source = Stopper::new();
//! do_work(&source).unwrap();
//! # }
//! # #[cfg(not(feature = "alloc"))]
//! # fn main() {}
//! ```
//!
//! ## Feature Flags
//!
//! This crate mirrors `enough`'s feature flags:
//!
//! - **`std`** (default) - Full functionality including timeouts
//! - **`alloc`** - Arc-based types, `into_boxed()`, `child()`, `StopDropRoll`
//! - **None** - Core trait and stack-based types only

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]
#![warn(clippy::all)]

#[cfg(feature = "alloc")]
extern crate alloc;

// Re-export everything from enough
pub use enough::*;

// Cancel guard module
#[cfg(feature = "alloc")]
mod guard;
#[cfg(feature = "alloc")]
pub use guard::{Cancellable, CancelGuard, StopDropRoll};

/// Extension trait providing ergonomic combinators for [`Stop`] implementations.
///
/// This trait is automatically implemented for all `Stop + Sized` types.
///
/// # Example
///
/// ```rust
/// use almost_enough::{StopSource, Stop, StopExt};
///
/// let source_a = StopSource::new();
/// let source_b = StopSource::new();
///
/// // Combine with .or()
/// let combined = source_a.as_ref().or(source_b.as_ref());
///
/// assert!(!combined.should_stop());
///
/// source_b.cancel();
/// assert!(combined.should_stop());
/// ```
pub trait StopExt: Stop + Sized {
    /// Combine this stop with another, stopping if either stops.
    ///
    /// This is equivalent to `OrStop::new(self, other)` but with a more
    /// ergonomic method syntax that allows chaining.
    ///
    /// # Example
    ///
    /// ```rust
    /// use almost_enough::{StopSource, Stop, StopExt};
    ///
    /// let timeout = StopSource::new();
    /// let cancel = StopSource::new();
    ///
    /// let combined = timeout.as_ref().or(cancel.as_ref());
    /// assert!(!combined.should_stop());
    ///
    /// cancel.cancel();
    /// assert!(combined.should_stop());
    /// ```
    ///
    /// # Chaining
    ///
    /// Multiple sources can be chained:
    ///
    /// ```rust
    /// use almost_enough::{StopSource, Stop, StopExt};
    ///
    /// let a = StopSource::new();
    /// let b = StopSource::new();
    /// let c = StopSource::new();
    ///
    /// let combined = a.as_ref().or(b.as_ref()).or(c.as_ref());
    ///
    /// c.cancel();
    /// assert!(combined.should_stop());
    /// ```
    #[inline]
    fn or<S: Stop>(self, other: S) -> OrStop<Self, S> {
        OrStop::new(self, other)
    }

    /// Convert this stop into a boxed trait object.
    ///
    /// This is useful for preventing monomorphization at API boundaries.
    /// Instead of generating a new function for each `impl Stop` type,
    /// you can erase the type to `BoxedStop` and have a single implementation.
    ///
    /// # Example
    ///
    /// ```rust
    /// # #[cfg(feature = "alloc")]
    /// # fn main() {
    /// use almost_enough::{Stopper, BoxedStop, Stop, StopExt};
    ///
    /// // This function is monomorphized for each Stop type
    /// fn process_generic(stop: impl Stop + 'static) {
    ///     // Erase type at boundary
    ///     process_concrete(stop.into_boxed());
    /// }
    ///
    /// // This function has only one implementation
    /// fn process_concrete(stop: BoxedStop) {
    ///     while !stop.should_stop() {
    ///         break;
    ///     }
    /// }
    ///
    /// let stop = Stopper::new();
    /// process_generic(stop);
    /// # }
    /// # #[cfg(not(feature = "alloc"))]
    /// # fn main() {}
    /// ```
    #[cfg(feature = "alloc")]
    #[inline]
    fn into_boxed(self) -> BoxedStop
    where
        Self: 'static,
    {
        BoxedStop::new(self)
    }

    /// Create a child stop that inherits cancellation from this stop.
    ///
    /// The returned [`ChildStopper`] will stop if:
    /// - Its own `cancel()` is called
    /// - This parent stop is cancelled
    ///
    /// Cancelling the child does NOT affect the parent.
    ///
    /// # Example
    ///
    /// ```rust
    /// # #[cfg(feature = "alloc")]
    /// # fn main() {
    /// use almost_enough::{Stopper, Stop, StopExt};
    ///
    /// let parent = Stopper::new();
    /// let child = parent.child();
    ///
    /// // Child cancellation is independent
    /// child.cancel();
    /// assert!(!parent.should_stop());
    /// assert!(child.should_stop());
    ///
    /// // Parent cancellation propagates
    /// let child2 = parent.child();
    /// parent.cancel();
    /// assert!(child2.should_stop());
    /// # }
    /// # #[cfg(not(feature = "alloc"))]
    /// # fn main() {}
    /// ```
    #[cfg(feature = "alloc")]
    #[inline]
    fn child(&self) -> ChildStopper
    where
        Self: Clone + 'static,
    {
        ChildStopper::with_parent(self.clone())
    }
}

// Blanket implementation for all Stop + Sized types
impl<T: Stop + Sized> StopExt for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn or_extension_works() {
        let a = StopSource::new();
        let b = StopSource::new();
        let combined = a.as_ref().or(b.as_ref());

        assert!(!combined.should_stop());

        a.cancel();
        assert!(combined.should_stop());
    }

    #[test]
    fn or_chain_works() {
        let a = StopSource::new();
        let b = StopSource::new();
        let c = StopSource::new();

        let combined = a.as_ref().or(b.as_ref()).or(c.as_ref());

        assert!(!combined.should_stop());

        c.cancel();
        assert!(combined.should_stop());
    }

    #[test]
    fn or_with_never() {
        let source = StopSource::new();
        let combined = Never.or(source.as_ref());

        assert!(!combined.should_stop());

        source.cancel();
        assert!(combined.should_stop());
    }

    #[test]
    fn reexports_work() {
        // Verify that re-exports from enough work
        let _: StopReason = StopReason::Cancelled;
        let _ = Never;
        let source = StopSource::new();
        let _ = source.as_ref();
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn alloc_reexports_work() {
        let stop = Stopper::new();
        let _ = stop.clone();
        let _ = BoxedStop::new(Never);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn into_boxed_works() {
        let stop = Stopper::new();
        let boxed: BoxedStop = stop.clone().into_boxed();

        assert!(!boxed.should_stop());

        stop.cancel();
        assert!(boxed.should_stop());
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn into_boxed_with_never() {
        let boxed: BoxedStop = Never.into_boxed();
        assert!(!boxed.should_stop());
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn into_boxed_prevents_monomorphization() {
        // This test verifies the pattern compiles correctly
        fn outer(stop: impl Stop + 'static) {
            inner(stop.into_boxed());
        }

        fn inner(stop: BoxedStop) {
            let _ = stop.should_stop();
        }

        let stop = Stopper::new();
        outer(stop);
        outer(Never);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn child_extension_works() {
        let parent = Stopper::new();
        let child = parent.child();

        assert!(!child.should_stop());

        parent.cancel();
        assert!(child.should_stop());
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn child_independent_cancel() {
        let parent = Stopper::new();
        let child = parent.child();

        child.cancel();

        assert!(child.should_stop());
        assert!(!parent.should_stop());
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn child_chain() {
        let grandparent = Stopper::new();
        let parent = grandparent.child();
        let child = parent.child();

        grandparent.cancel();

        assert!(parent.should_stop());
        assert!(child.should_stop());
    }
}

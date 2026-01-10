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
//! use almost_enough::{AtomicStop, Stop, StopExt};
//!
//! let timeout = AtomicStop::new();
//! let cancel = AtomicStop::new();
//!
//! // Combine: stop if either stops
//! let combined = timeout.token().or(cancel.token());
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
//! use almost_enough::{ArcStop, BoxStop, Stop, StopExt};
//!
//! fn outer(stop: impl Stop + 'static) {
//!     // Erase the concrete type to avoid monomorphizing inner()
//!     inner(stop.into_boxed());
//! }
//!
//! fn inner(stop: BoxStop) {
//!     // Only one version of this function exists
//!     while !stop.should_stop() {
//!         break;
//!     }
//! }
//!
//! let source = ArcStop::new();
//! outer(source.token());
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
//! use almost_enough::{ArcStop, StopDropRoll};
//!
//! fn do_work(source: &ArcStop) -> Result<(), &'static str> {
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
//! let source = ArcStop::new();
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
//! - **`alloc`** - Arc-based types, `into_boxed()`, `StopDropRoll`
//! - **None** - Core trait and atomic types only

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
/// use almost_enough::{AtomicStop, Stop, StopExt};
///
/// let source_a = AtomicStop::new();
/// let source_b = AtomicStop::new();
///
/// // Combine with .or()
/// let combined = source_a.token().or(source_b.token());
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
    /// use almost_enough::{AtomicStop, Stop, StopExt};
    ///
    /// let timeout = AtomicStop::new();
    /// let cancel = AtomicStop::new();
    ///
    /// let combined = timeout.token().or(cancel.token());
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
    /// use almost_enough::{AtomicStop, Stop, StopExt};
    ///
    /// let a = AtomicStop::new();
    /// let b = AtomicStop::new();
    /// let c = AtomicStop::new();
    ///
    /// let combined = a.token().or(b.token()).or(c.token());
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
    /// you can erase the type to `BoxStop` and have a single implementation.
    ///
    /// # Example
    ///
    /// ```rust
    /// # #[cfg(feature = "alloc")]
    /// # fn main() {
    /// use almost_enough::{ArcStop, BoxStop, Stop, StopExt};
    ///
    /// // This function is monomorphized for each Stop type
    /// fn process_generic(stop: impl Stop + 'static) {
    ///     // Erase type at boundary
    ///     process_concrete(stop.into_boxed());
    /// }
    ///
    /// // This function has only one implementation
    /// fn process_concrete(stop: BoxStop) {
    ///     while !stop.should_stop() {
    ///         break;
    ///     }
    /// }
    ///
    /// let source = ArcStop::new();
    /// process_generic(source.token());
    /// # }
    /// # #[cfg(not(feature = "alloc"))]
    /// # fn main() {}
    /// ```
    #[cfg(feature = "alloc")]
    #[inline]
    fn into_boxed(self) -> BoxStop
    where
        Self: 'static,
    {
        BoxStop::new(self)
    }
}

// Blanket implementation for all Stop + Sized types
impl<T: Stop + Sized> StopExt for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn or_extension_works() {
        let a = AtomicStop::new();
        let b = AtomicStop::new();
        let combined = a.token().or(b.token());

        assert!(!combined.should_stop());

        a.cancel();
        assert!(combined.should_stop());
    }

    #[test]
    fn or_chain_works() {
        let a = AtomicStop::new();
        let b = AtomicStop::new();
        let c = AtomicStop::new();

        let combined = a.token().or(b.token()).or(c.token());

        assert!(!combined.should_stop());

        c.cancel();
        assert!(combined.should_stop());
    }

    #[test]
    fn or_with_never() {
        let source = AtomicStop::new();
        let combined = Never.or(source.token());

        assert!(!combined.should_stop());

        source.cancel();
        assert!(combined.should_stop());
    }

    #[test]
    fn reexports_work() {
        // Verify that re-exports from enough work
        let _: StopReason = StopReason::Cancelled;
        let _ = Never;
        let source = AtomicStop::new();
        let _ = source.token();
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn alloc_reexports_work() {
        let source = ArcStop::new();
        let _ = source.token();
        let _ = BoxStop::new(Never);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn into_boxed_works() {
        let source = ArcStop::new();
        let token = source.token();
        let boxed: BoxStop = token.into_boxed();

        assert!(!boxed.should_stop());

        source.cancel();
        assert!(boxed.should_stop());
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn into_boxed_with_never() {
        let boxed: BoxStop = Never.into_boxed();
        assert!(!boxed.should_stop());
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn into_boxed_prevents_monomorphization() {
        // This test verifies the pattern compiles correctly
        fn outer(stop: impl Stop + 'static) {
            inner(stop.into_boxed());
        }

        fn inner(stop: BoxStop) {
            let _ = stop.should_stop();
        }

        let source = ArcStop::new();
        outer(source.token());
        outer(Never);
    }
}

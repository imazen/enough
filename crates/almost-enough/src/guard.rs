//! RAII guard for automatic cancellation on drop.
//!
//! This module provides [`CancelGuard`], which cancels a source when dropped
//! unless explicitly disarmed. This is useful for ensuring cleanup happens
//! on error paths or panics.
//!
//! # Example
//!
//! ```rust
//! use almost_enough::{Stopper, StopDropRoll};
//!
//! fn process(source: &Stopper) -> Result<(), &'static str> {
//!     // Guard will cancel on drop unless disarmed
//!     let guard = source.stop_on_drop();
//!
//!     // Do work that might fail...
//!     do_risky_work()?;
//!
//!     // Success - don't cancel
//!     guard.disarm();
//!     Ok(())
//! }
//!
//! fn do_risky_work() -> Result<(), &'static str> {
//!     Ok(())
//! }
//!
//! let source = Stopper::new();
//! process(&source).unwrap();
//! assert!(!source.is_cancelled()); // Not cancelled because we disarmed
//! ```

use crate::{Stopper, ChildStopper};

/// Trait for types that can be stopped/cancelled.
///
/// This is implemented for [`Stopper`] and [`ChildStopper`] to allow
/// creating [`CancelGuard`]s via the [`StopDropRoll`] trait.
///
/// The method is named `stop()` to align with the [`Stop`](crate::Stop) trait
/// and avoid conflicts with inherent `cancel()` methods.
pub trait Cancellable: Clone + Send {
    /// Request stop/cancellation.
    fn stop(&self);
}

impl Cancellable for Stopper {
    #[inline]
    fn stop(&self) {
        self.cancel();
    }
}

impl Cancellable for ChildStopper {
    #[inline]
    fn stop(&self) {
        self.cancel();
    }
}

/// A guard that cancels a source when dropped, unless disarmed.
///
/// This provides RAII-style cancellation for cleanup on error paths or panics.
/// Create one via the [`StopDropRoll`] trait.
///
/// # Example
///
/// ```rust
/// use almost_enough::{Stopper, StopDropRoll};
///
/// let source = Stopper::new();
///
/// {
///     let guard = source.stop_on_drop();
///     // guard dropped here, source gets cancelled
/// }
///
/// assert!(source.is_cancelled());
/// ```
///
/// # Disarming
///
/// Call [`disarm()`](Self::disarm) to prevent cancellation:
///
/// ```rust
/// use almost_enough::{Stopper, StopDropRoll};
///
/// let source = Stopper::new();
///
/// {
///     let guard = source.stop_on_drop();
///     guard.disarm(); // Prevents cancellation
/// }
///
/// assert!(!source.is_cancelled());
/// ```
#[derive(Debug)]
pub struct CancelGuard<C: Cancellable> {
    source: Option<C>,
}

impl<C: Cancellable> CancelGuard<C> {
    /// Create a new guard that will cancel the source on drop.
    ///
    /// Prefer using [`StopDropRoll::stop_on_drop()`] instead.
    #[inline]
    pub fn new(source: C) -> Self {
        Self {
            source: Some(source),
        }
    }

    /// Disarm the guard, preventing cancellation on drop.
    ///
    /// Call this when the guarded operation succeeds and you don't
    /// want to cancel.
    ///
    /// # Example
    ///
    /// ```rust
    /// use almost_enough::{Stopper, StopDropRoll};
    ///
    /// let source = Stopper::new();
    /// let guard = source.stop_on_drop();
    ///
    /// // Operation succeeded, don't cancel
    /// guard.disarm();  // Consumes guard, preventing cancellation
    ///
    /// assert!(!source.is_cancelled());
    /// ```
    #[inline]
    pub fn disarm(mut self) {
        self.source = None;
    }

    /// Check if this guard is still armed (will cancel on drop).
    #[inline]
    pub fn is_armed(&self) -> bool {
        self.source.is_some()
    }

    /// Get a reference to the underlying source, if still armed.
    #[inline]
    pub fn source(&self) -> Option<&C> {
        self.source.as_ref()
    }
}

impl<C: Cancellable> Drop for CancelGuard<C> {
    fn drop(&mut self) {
        if let Some(source) = self.source.take() {
            source.stop();
        }
    }
}

/// Extension trait for creating [`CancelGuard`]s.
///
/// This trait is implemented for types that support cancellation,
/// allowing you to create RAII guards that stop on drop.
///
/// # Supported Types
///
/// - [`Stopper`] - Stops all clones
/// - [`ChildStopper`] - Stops just this node (not siblings or parent)
///
/// # Example
///
/// ```rust
/// use almost_enough::{Stopper, StopDropRoll};
///
/// fn fallible_work(source: &Stopper) -> Result<i32, &'static str> {
///     let guard = source.stop_on_drop();
///
///     // If we return Err or panic, source is stopped
///     let result = compute()?;
///
///     // Success - don't stop
///     guard.disarm();
///     Ok(result)
/// }
///
/// fn compute() -> Result<i32, &'static str> {
///     Ok(42)
/// }
///
/// let source = Stopper::new();
/// assert_eq!(fallible_work(&source), Ok(42));
/// assert!(!source.is_cancelled());
/// ```
///
/// # With ChildStopper
///
/// ```rust
/// use almost_enough::{Stopper, ChildStopper, StopDropRoll, Stop, StopExt};
///
/// let parent = Stopper::new();
/// let child = parent.child();
///
/// {
///     let guard = child.stop_on_drop();
///     // guard dropped, child is stopped
/// }
///
/// assert!(child.is_cancelled());
/// assert!(!parent.is_cancelled()); // Parent is NOT affected
/// ```
pub trait StopDropRoll: Cancellable {
    /// Create a guard that will stop this source on drop.
    ///
    /// The guard can be disarmed via [`CancelGuard::disarm()`] to
    /// prevent stopping.
    fn stop_on_drop(&self) -> CancelGuard<Self>;
}

impl<C: Cancellable> StopDropRoll for C {
    #[inline]
    fn stop_on_drop(&self) -> CancelGuard<Self> {
        CancelGuard::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Stop, StopExt};

    #[test]
    fn guard_cancels_on_drop() {
        let source = Stopper::new();
        assert!(!source.is_cancelled());

        {
            let _guard = source.stop_on_drop();
        } // guard dropped here

        assert!(source.is_cancelled());
    }

    #[test]
    fn guard_disarm_prevents_cancel() {
        let source = Stopper::new();

        {
            let guard = source.stop_on_drop();
            guard.disarm();
        }

        assert!(!source.is_cancelled());
    }

    #[test]
    fn guard_is_armed() {
        let source = Stopper::new();
        let guard = source.stop_on_drop();

        assert!(guard.is_armed());
        guard.disarm();
        // After disarm, guard is consumed, so we can't check is_armed
    }

    #[test]
    fn guard_source_accessor() {
        let source = Stopper::new();
        let guard = source.stop_on_drop();

        assert!(guard.source().is_some());
    }

    #[test]
    fn guard_pattern_success() {
        fn work(source: &Stopper) -> Result<i32, &'static str> {
            let guard = source.stop_on_drop();
            let result = Ok(42);
            guard.disarm();
            result
        }

        let source = Stopper::new();
        assert_eq!(work(&source), Ok(42));
        assert!(!source.is_cancelled());
    }

    #[test]
    fn guard_pattern_failure() {
        fn work(source: &Stopper) -> Result<i32, &'static str> {
            let _guard = source.stop_on_drop();
            Err("failed")
            // guard dropped, source cancelled
        }

        let source = Stopper::new();
        assert_eq!(work(&source), Err("failed"));
        assert!(source.is_cancelled());
    }

    #[test]
    fn guard_multiple_clones() {
        let source = Stopper::new();
        let source2 = source.clone();

        {
            let _guard = source.stop_on_drop();
        }

        // Both clones see the cancellation
        assert!(source.is_cancelled());
        assert!(source2.is_cancelled());
    }

    #[test]
    fn guard_with_clone() {
        let source = Stopper::new();
        let clone = source.clone();

        assert!(!clone.should_stop());

        {
            let _guard = source.stop_on_drop();
        }

        assert!(clone.should_stop());
    }

    #[test]
    fn guard_tree_stopper() {
        let parent = Stopper::new();
        let child = parent.child();

        {
            let _guard = child.stop_on_drop();
        }

        // Child is cancelled
        assert!(child.is_cancelled());
        // Parent is NOT cancelled
        assert!(!parent.is_cancelled());
    }

    #[test]
    fn guard_tree_stopper_disarm() {
        let parent = Stopper::new();
        let child = parent.child();

        {
            let guard = child.stop_on_drop();
            guard.disarm();
        }

        assert!(!child.is_cancelled());
        assert!(!parent.is_cancelled());
    }
}

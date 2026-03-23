//! Arc-based cloneable dynamic dispatch for Stop.
//!
//! This module provides [`DynStop`], a shared-ownership wrapper that enables
//! dynamic dispatch without monomorphization bloat, with cheap `Clone`.
//!
//! # DynStop vs BoxedStop
//!
//! | | `DynStop` | `BoxedStop` |
//! |---|-----------|-------------|
//! | Clone | Yes (Arc increment) | No |
//! | Storage | `Arc<dyn Stop>` | `Box<dyn Stop>` |
//! | Send to threads | Clone and move | Must wrap in Arc yourself |
//! | Use case | Default choice | When Clone is unwanted |
//!
//! # Example
//!
//! ```rust
//! use almost_enough::{DynStop, Stopper, Unstoppable, Stop};
//!
//! let stopper = Stopper::new();
//! let stop = DynStop::new(stopper.clone());
//! let stop2 = stop.clone(); // Arc increment, no allocation
//!
//! stopper.cancel();
//! assert!(stop.should_stop());
//! assert!(stop2.should_stop());
//! ```

use alloc::sync::Arc;
use core::any::{Any, TypeId};

use crate::{Stop, StopReason};

/// A shared-ownership [`Stop`] implementation with cheap `Clone`.
///
/// Wraps any `Stop` in an `Arc` for shared ownership across threads.
/// Cloning is an atomic increment — no heap allocation.
///
/// # Indirection Collapsing
///
/// `DynStop::new()` detects when you pass another `DynStop` and unwraps
/// it instead of double-wrapping. `active_stop()` collapses to the inner
/// concrete type for hot loops.
///
/// # Example
///
/// ```rust
/// use almost_enough::{DynStop, Stopper, Stop, StopReason};
///
/// let stopper = Stopper::new();
/// let stop = DynStop::new(stopper.clone());
/// let stop2 = stop.clone(); // cheap Arc clone
///
/// stopper.cancel();
/// assert!(stop.should_stop());
/// assert!(stop2.should_stop()); // both see cancellation
/// ```
pub struct DynStop {
    inner: Arc<dyn Stop + Send + Sync>,
}

impl DynStop {
    /// Create a new `DynStop` from any [`Stop`] implementation.
    ///
    /// If `stop` is already a `DynStop`, it is unwrapped instead of
    /// double-wrapping (no extra indirection).
    #[inline]
    pub fn new<T: Stop + 'static>(stop: T) -> Self {
        // Collapse DynStop nesting: if T is DynStop, clone its inner Arc
        if TypeId::of::<T>() == TypeId::of::<DynStop>() {
            let any_ref: &dyn Any = &stop;
            let inner = any_ref.downcast_ref::<DynStop>().unwrap();
            let result = inner.clone();
            drop(stop);
            return result;
        }
        Self {
            inner: Arc::new(stop),
        }
    }

    /// Create a `DynStop` from an existing `Arc<T>` without re-wrapping.
    ///
    /// This is zero-cost — just widens the pointer. Use this when you
    /// already have an `Arc`-wrapped stop type.
    ///
    /// If `T` is `DynStop`, the inner Arc is extracted to avoid double
    /// indirection (`Arc<Arc<dyn Stop>>`).
    ///
    /// ```rust
    /// use almost_enough::{DynStop, Stopper, Stop};
    /// # #[cfg(feature = "std")]
    /// # fn main() {
    /// use std::sync::Arc;
    ///
    /// let stopper = Arc::new(Stopper::new());
    /// let stop = DynStop::from_arc(stopper); // pointer widening, no allocation
    /// assert!(!stop.should_stop());
    /// # }
    /// # #[cfg(not(feature = "std"))]
    /// # fn main() {}
    /// ```
    #[inline]
    pub fn from_arc<T: Stop + 'static>(arc: Arc<T>) -> Self {
        // Collapse Arc<DynStop> → reuse inner Arc
        if TypeId::of::<T>() == TypeId::of::<DynStop>() {
            let any_ref: &dyn Any = &*arc;
            let inner = any_ref.downcast_ref::<DynStop>().unwrap();
            return inner.clone();
        }
        Self {
            inner: arc as Arc<dyn Stop + Send + Sync>,
        }
    }

    /// Returns the effective inner stop if it may stop, collapsing indirection.
    ///
    /// The returned `&dyn Stop` points directly to the concrete type inside
    /// the `Arc`, bypassing the `DynStop` wrapper. In a hot loop, subsequent
    /// `check()` calls go through one vtable dispatch instead of two.
    ///
    /// Returns `None` if the inner stop is a no-op (e.g., `Unstoppable`).
    ///
    /// ```rust
    /// use almost_enough::{DynStop, Stopper, Unstoppable, Stop, StopReason};
    ///
    /// fn hot_loop(stop: &DynStop) -> Result<(), StopReason> {
    ///     let stop = stop.active_stop(); // Option<&dyn Stop>, collapsed
    ///     for i in 0..1000 {
    ///         stop.check()?;
    ///     }
    ///     Ok(())
    /// }
    ///
    /// assert!(hot_loop(&DynStop::new(Unstoppable)).is_ok());
    /// assert!(hot_loop(&DynStop::new(Stopper::new())).is_ok());
    /// ```
    #[inline]
    pub fn active_stop(&self) -> Option<&dyn Stop> {
        let inner: &dyn Stop = &*self.inner;
        if inner.may_stop() { Some(inner) } else { None }
    }
}

impl Clone for DynStop {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Stop for DynStop {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        self.inner.check()
    }

    #[inline]
    fn should_stop(&self) -> bool {
        self.inner.should_stop()
    }

    #[inline]
    fn may_stop(&self) -> bool {
        self.inner.may_stop()
    }
}

impl core::fmt::Debug for DynStop {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("DynStop").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FnStop, StopSource, Stopper, Unstoppable};

    #[test]
    fn from_unstoppable() {
        let stop = DynStop::new(Unstoppable);
        assert!(!stop.should_stop());
        assert!(stop.check().is_ok());
    }

    #[test]
    fn from_stopper() {
        let stopper = Stopper::new();
        let stop = DynStop::new(stopper.clone());

        assert!(!stop.should_stop());

        stopper.cancel();

        assert!(stop.should_stop());
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn clone_is_cheap() {
        let stopper = Stopper::new();
        let stop = DynStop::new(stopper.clone());
        let stop2 = stop.clone();

        stopper.cancel();

        // Both clones see the cancellation (shared state)
        assert!(stop.should_stop());
        assert!(stop2.should_stop());
    }

    #[cfg(feature = "std")]
    #[test]
    fn clone_send_to_thread() {
        let stopper = Stopper::new();
        let stop = DynStop::new(stopper.clone());

        let handle = std::thread::spawn({
            let stop = stop.clone();
            move || stop.should_stop()
        });

        stopper.cancel();
        // Thread may or may not see cancellation depending on timing
        let _ = handle.join().unwrap();
    }

    #[test]
    fn is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DynStop>();
    }

    #[test]
    fn debug_format() {
        let stop = DynStop::new(Unstoppable);
        let debug = alloc::format!("{:?}", stop);
        assert!(debug.contains("DynStop"));
    }

    #[test]
    fn may_stop_delegates() {
        assert!(!DynStop::new(Unstoppable).may_stop());
        assert!(DynStop::new(Stopper::new()).may_stop());
    }

    #[test]
    fn active_stop_unstoppable() {
        let stop = DynStop::new(Unstoppable);
        assert!(stop.active_stop().is_none());
    }

    #[test]
    fn active_stop_stopper() {
        let stopper = Stopper::new();
        let stop = DynStop::new(stopper.clone());

        let active = stop.active_stop();
        assert!(active.is_some());
        assert!(active.unwrap().check().is_ok());

        stopper.cancel();
        assert!(active.unwrap().should_stop());
    }

    #[test]
    fn collapses_nested_dyn_stop() {
        let inner = DynStop::new(Unstoppable);
        let outer = DynStop::new(inner);
        // Collapsed: outer wraps Arc<Unstoppable>, not Arc<DynStop<Arc<Unstoppable>>>
        assert!(!outer.may_stop());
        assert!(outer.active_stop().is_none());
    }

    #[test]
    fn collapses_nested_stopper() {
        let stopper = Stopper::new();
        let inner = DynStop::new(stopper.clone());
        let outer = DynStop::new(inner.clone());

        // Both share the same Arc chain
        stopper.cancel();
        assert!(outer.should_stop());
        assert!(inner.should_stop());
    }

    #[test]
    fn from_arc() {
        let stopper = Arc::new(Stopper::new());
        let cancel_handle = stopper.clone();
        let stop = DynStop::from_arc(stopper);

        assert!(!stop.should_stop());
        cancel_handle.cancel();
        assert!(stop.should_stop());
    }

    #[test]
    fn from_non_clone_fn_stop() {
        // FnStop with non-Clone closure — DynStop doesn't need Clone on T
        let flag = Arc::new(core::sync::atomic::AtomicBool::new(false));
        let flag2 = flag.clone();
        let stop = DynStop::new(FnStop::new(move || {
            flag2.load(core::sync::atomic::Ordering::Relaxed)
        }));

        assert!(!stop.should_stop());

        // Clone the DynStop (shares the Arc, not the closure)
        let stop2 = stop.clone();
        flag.store(true, core::sync::atomic::Ordering::Relaxed);

        assert!(stop.should_stop());
        assert!(stop2.should_stop());
    }

    #[test]
    fn avoids_monomorphization() {
        fn process(stop: DynStop) -> bool {
            stop.should_stop()
        }

        assert!(!process(DynStop::new(Unstoppable)));
        assert!(!process(DynStop::new(StopSource::new())));
        assert!(!process(DynStop::new(Stopper::new())));
    }

    #[test]
    fn hot_loop_pattern() {
        let stop = DynStop::new(Unstoppable);
        let active = stop.active_stop();
        for _ in 0..1000 {
            assert!(active.check().is_ok());
        }
    }
}

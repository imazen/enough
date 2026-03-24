//! Arc-based cloneable dynamic dispatch for Stop.
//!
//! This module provides [`StopToken`], a shared-ownership wrapper that enables
//! dynamic dispatch without monomorphization bloat, with cheap `Clone`.
//!
//! # StopToken vs BoxedStop
//!
//! | | `StopToken` | `BoxedStop` |
//! |---|-----------|-------------|
//! | Clone | Yes (Arc increment) | No |
//! | Storage | `Arc<dyn Stop>` | `Box<dyn Stop>` |
//! | Send to threads | Clone and move | Must wrap in Arc yourself |
//! | Use case | Default choice | When Clone is unwanted |
//!
//! # Example
//!
//! ```rust
//! use almost_enough::{StopToken, Stopper, Unstoppable, Stop};
//!
//! let stopper = Stopper::new();
//! let stop = StopToken::new(stopper.clone());
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
/// `StopToken::new()` detects when you pass another `StopToken` and unwraps
/// it instead of double-wrapping. No-op stops (`Unstoppable`) are stored
/// as `None` — `check()` short-circuits without any vtable dispatch.
///
/// # Example
///
/// ```rust
/// use almost_enough::{StopToken, Stopper, Stop, StopReason};
///
/// let stopper = Stopper::new();
/// let stop = StopToken::new(stopper.clone());
/// let stop2 = stop.clone(); // cheap Arc clone
///
/// stopper.cancel();
/// assert!(stop.should_stop());
/// assert!(stop2.should_stop()); // both see cancellation
/// ```
pub struct StopToken {
    inner: StopTokenInner,
}

/// Dispatch enum — avoids vtable for the common Stopper/SyncStopper cases.
enum StopTokenInner {
    /// No-op (Unstoppable). check() → Ok(()), no dispatch.
    None,
    /// Direct atomic load with Relaxed ordering (Stopper).
    Relaxed(Arc<crate::stopper::StopperInner>),
    /// Direct atomic load with Acquire ordering (SyncStopper).
    Acquire(Arc<crate::sync_stopper::SyncStopperInner>),
    /// Everything else — vtable dispatch.
    Dyn(Arc<dyn Stop + Send + Sync>),
}

impl StopToken {
    /// Create a new `StopToken` from any [`Stop`] implementation.
    ///
    /// If `stop` is already a `StopToken`, it is unwrapped instead of
    /// double-wrapping (no extra indirection).
    #[inline]
    pub fn new<T: Stop + 'static>(stop: T) -> Self {
        // Fast path: no-op stops skip all wrapping
        if !stop.may_stop() {
            return Self {
                inner: StopTokenInner::None,
            };
        }
        // Collapse StopToken nesting
        if TypeId::of::<T>() == TypeId::of::<StopToken>() {
            let any_ref: &dyn Any = &stop;
            let inner = any_ref.downcast_ref::<StopToken>().unwrap();
            let result = inner.clone();
            drop(stop);
            return result;
        }
        // Stopper: direct atomic, no vtable dispatch
        if TypeId::of::<T>() == TypeId::of::<crate::Stopper>() {
            let any_ref: &dyn Any = &stop;
            let stopper = any_ref.downcast_ref::<crate::Stopper>().unwrap();
            let result = Self {
                inner: StopTokenInner::Relaxed(stopper.inner.clone()),
            };
            drop(stop);
            return result;
        }
        // SyncStopper: direct atomic with Acquire ordering
        if TypeId::of::<T>() == TypeId::of::<crate::SyncStopper>() {
            let any_ref: &dyn Any = &stop;
            let stopper = any_ref.downcast_ref::<crate::SyncStopper>().unwrap();
            let result = Self {
                inner: StopTokenInner::Acquire(stopper.inner.clone()),
            };
            drop(stop);
            return result;
        }
        Self {
            inner: StopTokenInner::Dyn(Arc::new(stop)),
        }
    }

    /// Create a `StopToken` from an existing `Arc<T>` without re-wrapping.
    ///
    /// ```rust
    /// use almost_enough::{StopToken, Stopper, Stop};
    /// # #[cfg(feature = "std")]
    /// # fn main() {
    /// use std::sync::Arc;
    ///
    /// let stopper = Arc::new(Stopper::new());
    /// let stop = StopToken::from_arc(stopper);
    /// assert!(!stop.should_stop());
    /// # }
    /// # #[cfg(not(feature = "std"))]
    /// # fn main() {}
    /// ```
    #[inline]
    pub fn from_arc<T: Stop + 'static>(arc: Arc<T>) -> Self {
        if !arc.may_stop() {
            return Self {
                inner: StopTokenInner::None,
            };
        }
        if TypeId::of::<T>() == TypeId::of::<StopToken>() {
            let any_ref: &dyn Any = &*arc;
            let inner = any_ref.downcast_ref::<StopToken>().unwrap();
            return inner.clone();
        }
        Self {
            inner: StopTokenInner::Dyn(arc as Arc<dyn Stop + Send + Sync>),
        }
    }
}

impl Clone for StopTokenInner {
    #[inline]
    fn clone(&self) -> Self {
        match self {
            Self::None => Self::None,
            Self::Relaxed(arc) => Self::Relaxed(Arc::clone(arc)),
            Self::Acquire(arc) => Self::Acquire(Arc::clone(arc)),
            Self::Dyn(arc) => Self::Dyn(Arc::clone(arc)),
        }
    }
}

impl Clone for StopToken {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl Stop for StopToken {
    #[inline(always)]
    fn check(&self) -> Result<(), StopReason> {
        match &self.inner {
            StopTokenInner::None => Ok(()),
            StopTokenInner::Relaxed(inner) => inner.check(),
            StopTokenInner::Acquire(inner) => inner.check(),
            StopTokenInner::Dyn(inner) => inner.check(),
        }
    }

    #[inline(always)]
    fn should_stop(&self) -> bool {
        match &self.inner {
            StopTokenInner::None => false,
            StopTokenInner::Relaxed(inner) => inner.should_stop(),
            StopTokenInner::Acquire(inner) => inner.should_stop(),
            StopTokenInner::Dyn(inner) => inner.should_stop(),
        }
    }

    #[inline(always)]
    fn may_stop(&self) -> bool {
        !matches!(self.inner, StopTokenInner::None)
    }
}

/// Zero-cost conversion: reuses the Stopper's Arc. Direct atomic dispatch, no vtable.
impl From<crate::Stopper> for StopToken {
    #[inline]
    fn from(stopper: crate::Stopper) -> Self {
        Self {
            inner: StopTokenInner::Relaxed(stopper.inner),
        }
    }
}

/// Zero-cost conversion: reuses the SyncStopper's Arc. Direct atomic dispatch.
impl From<crate::SyncStopper> for StopToken {
    #[inline]
    fn from(stopper: crate::SyncStopper) -> Self {
        Self {
            inner: StopTokenInner::Acquire(stopper.inner),
        }
    }
}

impl core::fmt::Debug for StopToken {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("StopToken").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FnStop, StopSource, Stopper, Unstoppable};

    #[test]
    fn from_unstoppable() {
        let stop = StopToken::new(Unstoppable);
        assert!(!stop.should_stop());
        assert!(stop.check().is_ok());
    }

    #[test]
    fn from_stopper() {
        let stopper = Stopper::new();
        let stop = StopToken::new(stopper.clone());

        assert!(!stop.should_stop());

        stopper.cancel();

        assert!(stop.should_stop());
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn clone_is_cheap() {
        let stopper = Stopper::new();
        let stop = StopToken::new(stopper.clone());
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
        let stop = StopToken::new(stopper.clone());

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
        assert_send_sync::<StopToken>();
    }

    #[test]
    fn debug_format() {
        let stop = StopToken::new(Unstoppable);
        let debug = alloc::format!("{:?}", stop);
        assert!(debug.contains("StopToken"));
    }

    #[test]
    fn may_stop_delegates() {
        assert!(!StopToken::new(Unstoppable).may_stop());
        assert!(StopToken::new(Stopper::new()).may_stop());
    }

    #[test]
    fn unstoppable_is_none_internally() {
        let stop = StopToken::new(Unstoppable);
        assert!(!stop.may_stop());
        assert!(stop.check().is_ok());
    }

    #[test]
    fn collapses_nested_dyn_stop() {
        let inner = StopToken::new(Unstoppable);
        let outer = StopToken::new(inner);
        assert!(!outer.may_stop());
    }

    #[test]
    fn collapses_nested_stopper() {
        let stopper = Stopper::new();
        let inner = StopToken::new(stopper.clone());
        let outer = StopToken::new(inner.clone());

        // Both share the same Arc chain
        stopper.cancel();
        assert!(outer.should_stop());
        assert!(inner.should_stop());
    }

    #[test]
    fn from_arc() {
        let stopper = Arc::new(Stopper::new());
        let cancel_handle = stopper.clone();
        let stop = StopToken::from_arc(stopper);

        assert!(!stop.should_stop());
        cancel_handle.cancel();
        assert!(stop.should_stop());
    }

    #[test]
    fn from_non_clone_fn_stop() {
        // FnStop with non-Clone closure — StopToken doesn't need Clone on T
        let flag = Arc::new(core::sync::atomic::AtomicBool::new(false));
        let flag2 = flag.clone();
        let stop = StopToken::new(FnStop::new(move || {
            flag2.load(core::sync::atomic::Ordering::Relaxed)
        }));

        assert!(!stop.should_stop());

        // Clone the StopToken (shares the Arc, not the closure)
        let stop2 = stop.clone();
        flag.store(true, core::sync::atomic::Ordering::Relaxed);

        assert!(stop.should_stop());
        assert!(stop2.should_stop());
    }

    #[test]
    fn avoids_monomorphization() {
        fn process(stop: StopToken) -> bool {
            stop.should_stop()
        }

        assert!(!process(StopToken::new(Unstoppable)));
        assert!(!process(StopToken::new(StopSource::new())));
        assert!(!process(StopToken::new(Stopper::new())));
    }

    #[test]
    fn hot_loop_pattern() {
        let stop = StopToken::new(Unstoppable);
        for _ in 0..1000 {
            assert!(stop.check().is_ok()); // None path, no dispatch
        }
    }

    #[test]
    fn from_stopper_zero_cost() {
        let stopper = Stopper::new();
        let cancel = stopper.clone();
        let stop: StopToken = stopper.into(); // zero-cost: reuses Arc

        assert!(!stop.should_stop());
        cancel.cancel();
        assert!(stop.should_stop()); // same Arc, same AtomicBool
    }

    #[test]
    fn from_sync_stopper_zero_cost() {
        let stopper = crate::SyncStopper::new();
        let cancel = stopper.clone();
        let stop: StopToken = stopper.into();

        assert!(!stop.should_stop());
        cancel.cancel();
        assert!(stop.should_stop());
    }

    #[test]
    fn new_stopper_flattens() {
        // StopToken::new(Stopper) should reuse the Stopper's Arc,
        // not double-wrap in Arc<Stopper{Arc<AtomicBool>}>
        let stopper = Stopper::new();
        let cancel = stopper.clone();
        let stop = StopToken::new(stopper);

        cancel.cancel();
        assert!(stop.should_stop());
    }

    #[test]
    fn new_sync_stopper_flattens() {
        let stopper = crate::SyncStopper::new();
        let cancel = stopper.clone();
        let stop = StopToken::new(stopper);

        cancel.cancel();
        assert!(stop.should_stop());
    }

    #[test]
    fn from_arc_collapses_dynstop() {
        // from_arc(Arc<StopToken>) should reuse inner, not double-wrap
        let inner = StopToken::new(Stopper::new());
        let arc = alloc::sync::Arc::new(inner);
        let stop = StopToken::from_arc(arc);
        assert!(!stop.should_stop());
    }

    #[test]
    fn stopper_inner_debug() {
        let stop = Stopper::new();
        let debug = alloc::format!("{:?}", stop);
        assert!(debug.contains("cancelled"));
    }

    #[test]
    fn sync_stopper_inner_debug() {
        let stop = crate::SyncStopper::new();
        let debug = alloc::format!("{:?}", stop);
        assert!(debug.contains("cancelled"));
    }

    #[test]
    fn from_stopper_clone_shares_state() {
        let stopper = Stopper::new();
        let stop: StopToken = stopper.clone().into();
        let stop2 = stop.clone(); // Arc clone of the flattened inner

        stopper.cancel();
        // All three (stopper, stop, stop2) share the same AtomicBool
        assert!(stop.should_stop());
        assert!(stop2.should_stop());
    }
}

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
//! | Dispatch | Direct for `Stopper`, else vtable | Same (wraps a `StopToken`) |
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
/// `StopToken::new()` detects when you pass another `StopToken` (or a
/// [`BoxedStop`](crate::BoxedStop), which wraps one) and reuses it instead of
/// double-wrapping. No-op stops (`Unstoppable`) are stored as `None` —
/// `check()` short-circuits without any vtable dispatch. A `Stopper` is
/// checked as a direct atomic load, without a vtable. A `SyncStopper` keeps
/// its own `Arc` (no allocation) and is checked through the vtable.
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

/// Dispatch enum — avoids a vtable for the common `Stopper` case.
///
/// Keep it at three arms: with four, LLVM compiles the `match` in `check` to
/// a jump table, an indirect jump on every check on x86-64.
enum StopTokenInner {
    /// No-op (Unstoppable). check() → Ok(()), no dispatch.
    None,
    /// Direct atomic load with Relaxed ordering (Stopper).
    Relaxed(Arc<crate::stopper::StopperInner>),
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
        // A BoxedStop is a StopToken that can't be cloned: reuse it
        if let Some(token) = boxed_token(&stop) {
            return token;
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
        // SyncStopper: reuse its Arc behind the vtable, not Arc<SyncStopper>
        if TypeId::of::<T>() == TypeId::of::<crate::SyncStopper>() {
            let any_ref: &dyn Any = &stop;
            let stopper = any_ref.downcast_ref::<crate::SyncStopper>().unwrap();
            let result = Self {
                inner: StopTokenInner::Dyn(stopper.inner.clone()),
            };
            drop(stop);
            return result;
        }
        // An Arc or Box of `dyn Stop` becomes the Dyn arm itself instead of
        // the contents of another Arc: one indirect call per check, not two.
        match dyn_arc(stop) {
            Ok(arc) => Self {
                inner: StopTokenInner::Dyn(arc),
            },
            Err(stop) => Self {
                inner: StopTokenInner::Dyn(Arc::new(stop)),
            },
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
        // A Stopper's flag is checked directly, as in `new`. A SyncStopper
        // stays behind the vtable, where its checks are Acquire loads.
        if TypeId::of::<T>() == TypeId::of::<crate::Stopper>() {
            let any_ref: &dyn Any = &*arc;
            let stopper = any_ref.downcast_ref::<crate::Stopper>().unwrap();
            return Self {
                inner: StopTokenInner::Relaxed(stopper.inner.clone()),
            };
        }
        Self {
            inner: StopTokenInner::Dyn(arc as Arc<dyn Stop + Send + Sync>),
        }
    }
}

/// `stop` as the Dyn arm's `Arc` if it already is an `Arc` or `Box` of
/// `dyn Stop` (with or without `Send`/`Sync` spelled out); otherwise `stop`.
fn dyn_arc<T: Stop + 'static>(stop: T) -> Result<Arc<dyn Stop + Send + Sync>, T> {
    let mut slot = Some(stop);
    let any: &mut dyn Any = &mut slot;
    macro_rules! reuse_arc {
        ($arc:ty) => {
            if let Some(found) = any.downcast_mut::<Option<$arc>>() {
                let arc: Arc<dyn Stop + Send + Sync> = found.take().unwrap();
                return Ok(arc);
            }
        };
    }
    macro_rules! move_box {
        ($boxed:ty, $arc:ty) => {
            if let Some(found) = any.downcast_mut::<Option<$boxed>>() {
                let arc: $arc = Arc::from(found.take().unwrap());
                return Ok(arc);
            }
        };
    }
    reuse_arc!(Arc<dyn Stop + Send + Sync>);
    reuse_arc!(Arc<dyn Stop + Send>);
    reuse_arc!(Arc<dyn Stop>);
    move_box!(
        alloc::boxed::Box<dyn Stop + Send + Sync>,
        Arc<dyn Stop + Send + Sync>
    );
    move_box!(alloc::boxed::Box<dyn Stop + Send>, Arc<dyn Stop + Send>);
    move_box!(alloc::boxed::Box<dyn Stop>, Arc<dyn Stop>);
    Err(slot.take().unwrap())
}

/// The token inside `stop` if it is a (deprecated) `BoxedStop`.
#[allow(deprecated)]
#[inline]
fn boxed_token<T: Stop + 'static>(stop: &T) -> Option<StopToken> {
    let any_ref: &dyn Any = stop;
    any_ref
        .downcast_ref::<crate::BoxedStop>()
        .map(|boxed| boxed.0.clone())
}

impl Clone for StopTokenInner {
    #[inline]
    fn clone(&self) -> Self {
        match self {
            Self::None => Self::None,
            Self::Relaxed(arc) => Self::Relaxed(Arc::clone(arc)),
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
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        match &self.inner {
            StopTokenInner::None => Ok(()),
            StopTokenInner::Relaxed(inner) => inner.check(),
            StopTokenInner::Dyn(inner) => inner.check(),
        }
    }

    #[inline(always)]
    #[track_caller]
    fn should_stop(&self) -> bool {
        match &self.inner {
            StopTokenInner::None => false,
            StopTokenInner::Relaxed(inner) => inner.should_stop(),
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

/// Reuses the SyncStopper's Arc (no allocation); checks go through the vtable.
impl From<crate::SyncStopper> for StopToken {
    #[inline]
    fn from(stopper: crate::SyncStopper) -> Self {
        Self {
            inner: StopTokenInner::Dyn(stopper.inner),
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
    fn boxed_and_arced_dyn_stops_are_stops_with_almost_enough_alone() {
        // `alloc` turns on enough's `alloc`, which implements `Stop` for these.
        fn takes(stop: impl Stop + 'static) -> StopToken {
            StopToken::new(stop)
        }
        let arc: Arc<dyn Stop> = Arc::new(Stopper::new());
        assert!(takes(arc).check().is_ok());
        let boxed: alloc::boxed::Box<dyn Stop> = alloc::boxed::Box::new(Stopper::cancelled());
        assert!(takes(boxed).should_stop());
    }

    #[test]
    fn from_arc_checks_a_stopper_directly_and_a_sync_stopper_through_the_vtable() {
        let stopper = Stopper::new();
        let token = StopToken::from_arc(Arc::new(stopper.clone()));
        assert_eq!(direct(&token), "relaxed");
        assert!(!token.should_stop());
        stopper.cancel();
        assert!(token.should_stop());

        let sync = crate::SyncStopper::new();
        let token = StopToken::from_arc(Arc::new(sync.clone()));
        assert_eq!(direct(&token), "dyn");
        sync.cancel();
        assert!(token.should_stop());
    }

    #[test]
    fn an_arc_of_dyn_stop_is_the_dyn_arm_itself() {
        fn reused<A: Stop + Clone + 'static>(shared: A, data: *const ()) -> StopToken {
            let token = StopToken::new(shared);
            let StopTokenInner::Dyn(inner) = &token.inner else {
                panic!("{}", direct(&token));
            };
            assert_eq!(Arc::as_ptr(inner) as *const (), data);
            token
        }
        let stopper = Stopper::new();
        let plain: Arc<dyn Stop> = Arc::new(stopper.clone());
        let token = reused(plain.clone(), Arc::as_ptr(&plain) as *const ());
        let send: Arc<dyn Stop + Send> = Arc::new(stopper.clone());
        reused(send.clone(), Arc::as_ptr(&send) as *const ());
        let send_sync: Arc<dyn Stop + Send + Sync> = Arc::new(stopper.clone());
        reused(send_sync.clone(), Arc::as_ptr(&send_sync) as *const ());
        stopper.cancel();
        assert!(token.should_stop());
    }

    #[test]
    fn a_box_of_dyn_stop_moves_into_the_dyn_arm() {
        let stopper = Stopper::new();
        let boxed: alloc::boxed::Box<dyn Stop> = alloc::boxed::Box::new(stopper.clone());
        let token = StopToken::new(boxed);
        let StopTokenInner::Dyn(inner) = &token.inner else {
            panic!("{}", direct(&token));
        };
        // The Arc holds the Stopper, not a Box around it.
        assert_eq!(
            core::mem::size_of_val(&**inner),
            core::mem::size_of::<Stopper>()
        );
        stopper.cancel();
        assert!(token.should_stop());
    }

    fn direct(stop: &StopToken) -> &'static str {
        match &stop.inner {
            StopTokenInner::None => "none",
            StopTokenInner::Relaxed(_) => "relaxed",
            StopTokenInner::Dyn(_) => "dyn",
        }
    }

    #[test]
    #[allow(deprecated)] // Exercises the deprecated BoxedStop.
    fn boxed_stop_takes_the_same_fast_paths() {
        use crate::{BoxedStop, SyncStopper};
        assert_eq!(direct(&BoxedStop::new(Unstoppable).0), "none");
        let stopper = Stopper::new();
        let boxed = BoxedStop::new(stopper.clone());
        assert_eq!(direct(&boxed.0), "relaxed");
        let StopTokenInner::Relaxed(inner) = &boxed.0.inner else {
            unreachable!()
        };
        assert!(
            Arc::ptr_eq(inner, &stopper.inner),
            "reuses the Stopper's Arc"
        );
        let sync = SyncStopper::new();
        let boxed = BoxedStop::new(sync.clone());
        assert_eq!(direct(&boxed.0), "dyn");
        let StopTokenInner::Dyn(inner) = &boxed.0.inner else {
            unreachable!()
        };
        assert_eq!(
            Arc::as_ptr(inner).cast::<()>(),
            Arc::as_ptr(&sync.inner).cast::<()>(),
            "reuses the SyncStopper's Arc"
        );
        assert_eq!(direct(&BoxedStop::new(StopSource::new()).0), "dyn");
    }

    #[test]
    #[allow(deprecated)] // Exercises the deprecated BoxedStop.
    fn boxed_stop_and_stop_token_nest_without_wrapping() {
        use crate::BoxedStop;
        let token = StopToken::new(FnStop::new(|| false));
        let StopTokenInner::Dyn(original) = &token.inner else {
            unreachable!()
        };
        let boxed = BoxedStop::new(token.clone());
        let again = BoxedStop::new(boxed);
        let back = StopToken::new(again);
        let StopTokenInner::Dyn(inner) = &back.inner else {
            unreachable!()
        };
        assert!(Arc::ptr_eq(inner, original));
    }

    #[test]
    #[allow(deprecated)] // Exercises the deprecated BoxedStop.
    fn boxed_stop_sees_cancellation_through_every_path() {
        use crate::{BoxedStop, SyncStopper};
        let stopper = Stopper::new();
        let sync = SyncStopper::new();
        let boxed = [
            BoxedStop::new(stopper.clone()),
            BoxedStop::new(sync.clone()),
            BoxedStop::new(BoxedStop::new(StopToken::new(stopper.clone()))),
        ];
        assert!(boxed.iter().all(|stop| stop.check().is_ok()));
        stopper.cancel();
        sync.cancel();
        assert!(boxed.iter().all(|stop| stop.should_stop()));
        assert_eq!(boxed[0].check(), Err(StopReason::Cancelled));
    }

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
    fn from_sync_stopper_reuses_its_arc_behind_the_vtable() {
        let stopper = crate::SyncStopper::new();
        let cancel = stopper.clone();
        let stop: StopToken = stopper.into();
        assert_eq!(direct(&stop), "dyn");
        let StopTokenInner::Dyn(inner) = &stop.inner else {
            unreachable!()
        };
        assert_eq!(
            Arc::as_ptr(inner).cast::<()>(),
            Arc::as_ptr(&cancel.inner).cast::<()>()
        );

        assert!(!stop.should_stop());
        cancel.cancel();
        assert!(stop.should_stop());
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
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

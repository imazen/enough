//! [`SyncStopper`], deprecated: [`Stopper`](crate::Stopper) now has the same
//! Release/Acquire ordering, so `SyncStopper` wraps one.

#![allow(deprecated)] // This module defines and tests the deprecated type.

use crate::{Stop, StopReason, Stopper};

/// A [`Stopper`]. Deprecated: `Stopper` now has the same Release/Acquire
/// ordering that set `SyncStopper` apart, so `SyncStopper` wraps one and
/// behaves identically.
///
/// A thread that sees the stop also sees every write made before `cancel()`;
/// see [`Stopper`]'s docs.
#[deprecated(
    since = "0.4.5",
    note = "`Stopper` now has the same Release/Acquire ordering; use it"
)]
#[derive(Debug, Clone, Default)]
pub struct SyncStopper(pub(crate) Stopper);

impl SyncStopper {
    /// Create a new stopper.
    #[inline]
    pub fn new() -> Self {
        Self(Stopper::new())
    }

    /// Create a stopper that is already cancelled.
    #[inline]
    pub fn cancelled() -> Self {
        Self(Stopper::cancelled())
    }

    /// Cancel, with Release ordering: see [`Stopper::cancel`].
    #[inline]
    pub fn cancel(&self) {
        self.0.cancel();
    }

    /// Whether it was cancelled, with Acquire ordering: see
    /// [`Stopper::is_cancelled`].
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

impl Stop for SyncStopper {
    #[inline]
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.0.check()
    }

    #[inline]
    #[track_caller]
    fn should_stop(&self) -> bool {
        self.0.should_stop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_stopper_basic() {
        let stop = SyncStopper::new();
        assert!(!stop.is_cancelled());
        assert!(!stop.should_stop());
        assert!(stop.check().is_ok());

        stop.cancel();

        assert!(stop.is_cancelled());
        assert!(stop.should_stop());
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn sync_stopper_cancelled_constructor() {
        let stop = SyncStopper::cancelled();
        assert!(stop.is_cancelled());
        assert!(stop.should_stop());
    }

    #[test]
    fn sync_stopper_clone_shares_state() {
        let stop1 = SyncStopper::new();
        let stop2 = stop1.clone();

        stop2.cancel();

        assert!(stop1.should_stop());
        assert!(stop2.should_stop());
    }

    #[test]
    fn sync_stopper_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SyncStopper>();
    }

    #[test]
    fn sync_stopper_is_default() {
        let stop: SyncStopper = Default::default();
        assert!(!stop.is_cancelled());
    }
}

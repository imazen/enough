//! Consumer-owned progress tracking, polling, and execution profiling.
//!
//! Library signatures depend on [`howfar::Report`] and [`enough::Stop`].
//! Applications and tests opt into this crate to create trees and callbacks.
#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;
pub use enough::{Stop, StopReason, Unstoppable};
pub use howfar::{IgnoreProgress, Report};
pub mod ext;
mod json;
pub mod poll;
#[cfg(feature = "profile")]
pub mod profile;
mod sync;
mod tree;
pub use tree::{
    Execution, Observer, Outcome, Part, Phase, PlanError, Progress, Snapshot, Status, Total,
};

/// Carry cancellation and reporting together, without coupling their cadence.
#[derive(Clone, Copy, Debug)]
pub struct Work<S, R> {
    /// The cancellation policy (including any explicitly chosen polling hook).
    pub stop: S,
    /// The completed-work sink.
    pub progress: R,
}

impl<S, R> Work<S, R> {
    /// Pair any stop policy with any progress sink.
    pub const fn new(stop: S, progress: R) -> Self {
        Self { stop, progress }
    }
}

impl<S: Stop, R: Send + Sync> Stop for Work<S, R> {
    #[inline]
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.stop.check()
    }
    #[inline]
    fn may_stop(&self) -> bool {
        self.stop.may_stop()
    }
}

impl<S: Send + Sync, R: Report> Report for Work<S, R> {
    #[inline]
    #[track_caller]
    fn advance(&self, completed: u64) {
        self.progress.advance(completed);
    }
    #[inline]
    fn may_report(&self) -> bool {
        self.progress.may_report()
    }
}

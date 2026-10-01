//! Consumer-owned progress tracking, polling, and execution profiling.
//!
//! Libraries can accept one [`how_far::Pulse`] for cancellation, reporting, and
//! nested phase planning. Applications and tests use [`PulseTree`] to supply
//! that interface with trees, observers, callbacks, and profiling.
#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;
pub use enough::{Stop, StopReason, Unstoppable};
pub use how_far::{IgnoreProgress, ProgressExt, Report};
#[cfg(feature = "diagnostics")]
pub mod diagnostics;
pub mod ext;
mod json;
pub mod poll;
#[cfg(feature = "profile")]
pub mod profile;
mod pulse;
mod sync;
mod tree;
pub use how_far::{NoPulse, PhaseSpec, Pulse};
pub use pulse::PulseTree;
pub use tree::{
    Execution, Observer, Outcome, Part, Phase, PlanError, Progress, Snapshot, Status, Total,
};

/// Pair a stop policy with a progress sink. Plain `check()` only checks;
/// [`ProgressExt::step`] reports and checks at a completed-work checkpoint.
#[derive(Clone, Copy, Debug)]
pub struct ProgressWithStop<S, R> {
    /// The cancellation policy (including any explicitly chosen polling hook).
    pub stop: S,
    /// The completed-work sink.
    pub progress: R,
}

impl<S, R> ProgressWithStop<S, R> {
    /// Pair any stop policy with any progress sink.
    pub const fn new(stop: S, progress: R) -> Self {
        Self { stop, progress }
    }
}

impl<S: Stop, R: Send + Sync> Stop for ProgressWithStop<S, R> {
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

impl<S: Send + Sync, R: Report> Report for ProgressWithStop<S, R> {
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

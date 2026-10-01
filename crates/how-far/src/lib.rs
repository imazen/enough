//! One small cancellation-and-progress interface for library authors.
//!
//! Accept [`Pulse`] to let a library declare nested work through one callback;
//! the caller chooses whether to ignore, count, display, or profile it.
//! This crate uses `no_std + alloc`, has one dependency on `enough`, and has
//! no feature flags. Reporting itself never allocates.
//!
//! ```
//! use how_far::{IgnoreProgress, Report};
//!
//! fn process(rows: &[u8], progress: &impl Report) {
//!     for chunk in rows.chunks(16) {
//!         // Successfully process the chunk, then report the actual row count.
//!         progress.advance(chunk.len() as u64);
//!     }
//! }
//! process(&[0; 17], &IgnoreProgress);
//! ```
//!
//! Applications can opt into `how-far-along` for shared counters, weighted trees,
//! callbacks, and profiling. A library only needs this crate; it re-exports
//! [`Stop`] and [`StopReason`] from `enough`.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

pub use enough::{Stop, StopReason};

mod pulse;
mod steps;
pub use pulse::{Execution, NoPulse, Outcome, PhaseSpec, PlanError, Pulse, Total};
pub use steps::{RunError, Steps};

/// Report completed units and check cancellation at the same checkpoint.
///
/// `check()` remains the cheapest stop-only path. `step()` is the usual choice
/// after making progress: it records units already completed before observing
/// a stop request. Check once before starting a loop as well.
pub trait ProgressExt: Stop + Report {
    /// Record completed units, then check for a stop request.
    #[inline]
    #[track_caller]
    fn step(&self, completed: u64) -> Result<(), StopReason> {
        self.advance(completed);
        self.check()
    }
}
impl<T: Stop + Report + ?Sized> ProgressExt for T {}

/// A sink for **completed** units. `advance` only reports; when the value also
/// implements [`Stop`], use [`ProgressExt::step`] for a reporting checkpoint
/// that checks cancellation too.
///
/// Implementations can be shared by workers. `advance` may run on any of their
/// threads. Time budgets, batching and callback dispatch belong to adapters.
pub trait Report: Send + Sync {
    /// Add completed units. Use the actual count for partial final batches.
    #[track_caller]
    fn advance(&self, completed: u64);

    /// Whether reporting has observable effects. Only permanent no-ops return false.
    fn may_report(&self) -> bool {
        true
    }
}

/// Discard progress reports. This zero-sized sink optimizes away in generic code.
#[derive(Clone, Copy, Debug, Default)]
pub struct IgnoreProgress;
impl Report for IgnoreProgress {
    #[inline(always)]
    fn advance(&self, _: u64) {}
    #[inline(always)]
    fn may_report(&self) -> bool {
        false
    }
}

impl<T: Report + ?Sized> Report for &T {
    #[inline]
    #[track_caller]
    fn advance(&self, completed: u64) {
        (**self).advance(completed);
    }
    #[inline]
    fn may_report(&self) -> bool {
        (**self).may_report()
    }
}

impl<T: Report + ?Sized> Report for &mut T {
    #[inline]
    #[track_caller]
    fn advance(&self, completed: u64) {
        (**self).advance(completed);
    }
    #[inline]
    fn may_report(&self) -> bool {
        (**self).may_report()
    }
}

impl<T: Report + ?Sized> Report for alloc::boxed::Box<T> {
    #[inline]
    #[track_caller]
    fn advance(&self, completed: u64) {
        (**self).advance(completed);
    }
    #[inline]
    fn may_report(&self) -> bool {
        (**self).may_report()
    }
}

impl<T: Report + ?Sized> Report for alloc::sync::Arc<T> {
    #[inline]
    #[track_caller]
    fn advance(&self, completed: u64) {
        (**self).advance(completed);
    }
    #[inline]
    fn may_report(&self) -> bool {
        (**self).may_report()
    }
}

impl<T: Report> Report for Option<T> {
    #[inline]
    #[track_caller]
    fn advance(&self, completed: u64) {
        if let Some(report) = self {
            report.advance(completed);
        }
    }
    #[inline]
    fn may_report(&self) -> bool {
        self.as_ref().is_some_and(Report::may_report)
    }
}

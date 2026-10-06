//! Counting completed work.

use crate::{Stop, StopReason};
use alloc::{boxed::Box, sync::Arc};

/// A sink for **completed** units of work.
///
/// `advance` only counts. It never blocks, polls, or reads a clock, so it is
/// safe in hot loops and on any thread; a phase shared by workers receives
/// reports from all of them. Report the actual number of finished units,
/// including a short final batch.
pub trait Report: Send + Sync {
    /// Add `completed` finished units.
    #[track_caller]
    fn advance(&self, completed: u64);

    /// Whether reports have any effect.
    ///
    /// Only sinks that permanently discard reports return `false`. A caller
    /// may skip work that exists only to compute a count.
    fn may_report(&self) -> bool {
        true
    }
}

/// Discards every report. It is zero-sized and compiles away in generic code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NoReport;

impl Report for NoReport {
    #[inline(always)]
    fn advance(&self, _: u64) {}
    #[inline(always)]
    fn may_report(&self) -> bool {
        false
    }
}

/// Forward `Report` through a pointer-like wrapper.
macro_rules! forward_report {
    ($($wrapper:ty),*) => {$(
        impl<T: Report + ?Sized> Report for $wrapper {
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
    )*};
}
forward_report!(&T, &mut T, Box<T>, Arc<T>);

/// `None` discards reports; `Some` forwards them.
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

/// A stop policy and a progress sink used as one value.
///
/// `check` goes to `stop` and `advance` goes to `report`, so the pair works
/// wherever `Stop + Report` is expected, including
/// [`ProgressExt::step`](crate::ProgressExt::step). `ProgressWithStop::new(Unstoppable, NoReport)`
/// is zero-sized.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ProgressWithStop<S, R> {
    /// The cancellation policy.
    pub stop: S,
    /// The completed-work sink.
    pub report: R,
}

impl<S, R> ProgressWithStop<S, R> {
    /// Pair a stop policy with a progress sink.
    pub const fn new(stop: S, report: R) -> Self {
        Self { stop, report }
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
        self.report.advance(completed);
    }
    #[inline]
    fn may_report(&self) -> bool {
        self.report.may_report()
    }
}

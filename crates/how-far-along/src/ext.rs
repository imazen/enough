//! Report adapters. Importing this trait is optional.

use crate::Report;
use core::num::NonZeroU64;

/// Adapters for any [`Report`] sink.
pub trait ReportExt: Report {
    /// Buffer one worker's reports, publishing them in batches of at least
    /// `threshold` units, and the remainder on [`Batch::flush`] or drop.
    ///
    /// Batching reduces contention on a counter shared by many workers. It
    /// never delays cancellation, because a batch only counts. Flush or drop
    /// every batch before finishing the phase.
    fn batched(self, threshold: NonZeroU64) -> Batch<Self>
    where
        Self: Sized,
    {
        Batch {
            report: self,
            threshold,
            pending: 0,
        }
    }
}
impl<T: Report + ?Sized> ReportExt for T {}

/// One worker's report buffer, from [`ReportExt::batched`].
///
/// It is single-owner by design: `advance` takes `&mut self`. Give each worker
/// its own batch.
#[derive(Debug)]
pub struct Batch<R: Report> {
    report: R,
    threshold: NonZeroU64,
    pending: u64,
}
impl<R: Report> Batch<R> {
    /// Buffer `completed` finished units, publishing once the threshold is met.
    /// A sum that would overflow is published first, so counts never wrap.
    #[track_caller]
    pub fn advance(&mut self, completed: u64) {
        if let Some(sum) = self.pending.checked_add(completed) {
            self.pending = sum;
        } else {
            self.flush();
            self.pending = completed;
        }
        if self.pending >= self.threshold.get() {
            self.flush();
        }
    }
    /// Publish every buffered unit. Nothing is reported when none are pending.
    #[track_caller]
    pub fn flush(&mut self) {
        if self.pending != 0 {
            self.report.advance(core::mem::take(&mut self.pending));
        }
    }
    /// Units buffered but not yet visible to observers.
    pub fn pending(&self) -> u64 {
        self.pending
    }
}
impl<R: Report> Drop for Batch<R> {
    fn drop(&mut self) {
        self.flush();
    }
}

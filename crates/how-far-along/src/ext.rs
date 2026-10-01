//! Optional conveniences. The basic traits remain just `check` and `advance`.

use crate::Report;
use core::num::NonZeroU64;

/// Report-and-check convenience from the lightweight interface crate.
pub use how_far::ProgressExt;

/// Adapters for sinks; importing this trait is optional.
pub trait ReportExt: Report {
    /// Buffer this worker's reports, flushing at a unit threshold and on drop.
    ///
    /// This never batches cancellation. Flush (or drop) before joining/finishing
    /// the phase. Use one batch per worker to avoid a shared counter's contention.
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

/// A single-writer buffer. Its mutable methods make ownership explicit.
pub struct Batch<R: Report> {
    report: R,
    threshold: NonZeroU64,
    pending: u64,
}
impl<R: Report> Batch<R> {
    /// Buffer actual completed units, flushing without wrapping the count.
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
    /// Publish the remaining units. Zero pending units generate no report.
    #[track_caller]
    pub fn flush(&mut self) {
        if self.pending != 0 {
            self.report.advance(core::mem::take(&mut self.pending));
        }
    }
    /// Units not yet visible to observers.
    pub fn pending(&self) -> u64 {
        self.pending
    }
}
impl<R: Report> Drop for Batch<R> {
    fn drop(&mut self) {
        self.flush();
    }
}

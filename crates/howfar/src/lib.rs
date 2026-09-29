//! Progress you can count, cancellation you can check.
//!
//! Libraries accept [`Report`] alongside [`Stop`]. [`Work`] combines them when
//! both travel together. Neither trait reads a clock, allocates, or dispatches
//! callbacks by itself. Cancellation checkpoints and completed units are
//! independent: checking before work must not count work that never happened.
//!
//! ```
//! use howfar::{NoProgress, Report, Stop, StopReason, Unstoppable, Work};
//! use howfar::ext::WorkExt;
//!
//! fn process(rows: &[u8], work: impl Stop + Report) -> Result<(), StopReason> {
//!     work.check()?;
//!     for chunk in rows.chunks(16) {
//!         // Process the chunk here, then report its actual length.
//!         work.step(chunk.len() as u64)?;
//!     }
//!     Ok(())
//! }
//! process(&[0; 17], Work::new(Unstoppable, NoProgress))?;
//! # Ok::<(), StopReason>(())
//! ```
//!
//! With `alloc`, `Phase` owns the lifecycle, `Progress` is a clonable worker
//! handle, and `Observer` reads snapshots. Declare weighted children with
//! `Phase::split`; use `poll` for callbacks and cooperative yield requests.
//! With `profile`, the `profile` module records per-task checkpoint gaps and overlapping
//! execution spans using a consumer-selected clock.
//!
//! Feature layers: no features = core traits; `alloc` = trees and polling;
//! `std` (default) = standard-library support; `profile` = opt-in instrumentation
//! with OS mutexes and an injected clock (requires `std`). Rust 1.88+. No runtime,
//! proc macros, serialization framework, or scheduler dependency.
#![cfg_attr(not(feature = "std"), no_std)]
#![deny(unsafe_code)]
#![warn(missing_docs)]

#[cfg(feature = "alloc")]
extern crate alloc;

pub use enough::{Stop, StopReason, Unstoppable};

pub mod ext;
#[cfg(feature = "alloc")]
mod json;
#[cfg(feature = "alloc")]
pub mod poll;
#[cfg(feature = "profile")]
pub mod profile;
#[cfg(feature = "alloc")]
#[allow(unsafe_code)] // Audited immutable publication primitive; see sync.rs.
mod sync;
#[cfg(feature = "alloc")]
mod tree;
#[cfg(feature = "alloc")]
pub use tree::{
    Execution, Observer, Outcome, Part, Phase, PlanError, Progress, Snapshot, Status, Total,
};

/// A sink for **completed** units. Reporting does not check cancellation.
///
/// Implementations can be shared by workers. `advance` may run on any of their
/// threads. Time budgets, batching and observer dispatch are separate adapters.
pub trait Report: Send + Sync {
    /// Add completed units. Use the actual count for partial final batches.
    #[track_caller]
    fn advance(&self, completed: u64);

    /// Whether reporting has observable effects. Only permanent no-ops return false.
    fn may_report(&self) -> bool {
        true
    }
}

/// An allocation-free progress sink that optimizes away in generic code.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoProgress;

impl Report for NoProgress {
    #[inline(always)]
    fn advance(&self, _: u64) {}
    #[inline(always)]
    fn may_report(&self) -> bool {
        false
    }
}

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

macro_rules! forward_report {
    ($wrapper:ty) => {
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
    };
}
forward_report!(&T);
forward_report!(&mut T);
#[cfg(feature = "alloc")]
forward_report!(alloc::boxed::Box<T>);
#[cfg(feature = "alloc")]
forward_report!(alloc::sync::Arc<T>);

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

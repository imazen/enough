//! One cancellation-and-progress interface for libraries.
//!
//! A library accepts `&dyn Pulse`. Through it the library checks for
//! cancellation, reports completed work, and declares weighted phases. The
//! caller decides what happens to those reports: nothing ([`NoPulse`]), one
//! callback that can also stop the work ([`FnPulse`]), a live tree with
//! snapshots and callbacks (the `how-far-along` crate), or its own [`Pulse`]
//! implementation.
//!
//! ```
//! use how_far::{PhaseSpec, ProgressExt, Pulse, RunError, Stages, StopReason, Total};
//!
//! fn convert(rows: &[u8], pulse: &dyn Pulse) -> Result<(), RunError<StopReason>> {
//!     let mut stages = Stages::new(pulse, &[
//!         PhaseSpec::new("decode", 3, Total::Exact(rows.len() as u64)).units("rows"),
//!         PhaseSpec::new("encode", 1, Total::Exact(1)),
//!     ])?;
//!     stages.run_stoppable(|stage| {
//!         stage.check()?;
//!         for _row in rows {
//!             // Decode the row, then count it.
//!             stage.step(1)?;
//!         }
//!         Ok(())
//!     })?;
//!     stages.run_stoppable(|stage| stage.step(1))?;
//!     stages.finish()?;
//!     Ok(())
//! }
//!
//! convert(&[0; 4], &how_far::NoPulse)?;
//! # Ok::<(), RunError<StopReason>>(())
//! ```
//!
//! Three rules keep libraries composable:
//!
//! - **Finish what you split, never what you were given.** [`Pulse::split`]
//!   returns owned [`Child`] handles; only their owner can finish them. The
//!   pulse a library receives belongs to its caller.
//! - **Report completed work.** [`Report::advance`] counts units that are done,
//!   including a short final batch. [`ProgressExt::step`] counts and then checks
//!   for cancellation.
//! - **Borrow in hot paths, own in `'static` code.** `&dyn Pulse` costs two
//!   words. Work that must own its stop policy or progress sink, such as a
//!   spawned thread or a codec context, takes [`Pulse::handle`].
//!
//! The crate is `no_std + alloc`, has no feature flags, and depends only on
//! `enough`, whose [`Stop`] trait it re-exports.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

pub use enough::{Stop, StopReason, Unstoppable};

mod ext;
// Its counters are 64-bit atomics, which some embedded targets lack.
#[cfg(target_has_atomic = "64")]
mod fn_pulse;
mod paced;
mod pulse;
mod report;
mod stages;

pub use ext::ProgressExt;
#[cfg(target_has_atomic = "64")]
pub use fn_pulse::{FnPulse, Progress};
pub use paced::Paced;
pub use pulse::{
    Child, ChildPulse, Execution, Inert, NoPulse, Outcome, PhaseSpec, PlanError, Pulse,
    PulseHandle, Total,
};
pub use report::{NoReport, ProgressWithStop, Report};
pub use stages::{RunError, Stages};

/// The traits whose methods library code calls: `use how_far::prelude::*;`.
///
/// Trait methods need their trait in scope. `&dyn Pulse` brings its own, but
/// values such as [`ProgressExt::live`]'s `Option` or a [`PulseHandle`] need
/// `Stop`, `Report` and `ProgressExt` imported to call `check`, `advance` and
/// `step`.
pub mod prelude {
    pub use crate::{ProgressExt, Pulse, Report, Stop};
}

/// The README's examples, compiled and run as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

#![doc = include_str!("../README.md")]
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

pub use enough::{Stop, StopReason, Unstoppable};

#[cfg(feature = "adapters")]
mod adapters;
#[cfg(feature = "adapters")]
pub use adapters::WithStop;
mod completion;
mod ext;
mod paced;
mod phases;
mod pulse;
mod report;
mod stages;
mod stop_only;
#[cfg(feature = "checked")]
mod try_stages;

pub use completion::{Complete, IsStop, ResultExt};
pub use ext::ProgressExt;
pub use paced::Paced;
pub use phases::Phases;
pub use pulse::{
    Child, ChildPulse, Execution, Inert, NoPulse, Outcome, PhaseSpec, PlanError, Pulse,
    SharedPulse, Total,
};
pub use report::{NoReport, ProgressWithStop, Report};
pub use stages::Stages;
pub use stop_only::StopOnly;
#[cfg(feature = "checked")]
pub use try_stages::{RunError, TryStages};

/// The traits whose methods library code calls: `use how_far::prelude::*;`.
///
/// Trait methods need their trait in scope. `&dyn Pulse` brings its own, but
/// values such as [`ProgressExt::live`]'s `Option` or a [`SharedPulse`] need
/// `Stop`, `Report` and `ProgressExt` imported to call `check`, `advance` and
/// `step`.
pub mod prelude {
    pub use crate::{Complete, IsStop, ProgressExt, Pulse, Report, ResultExt, Stop};
}

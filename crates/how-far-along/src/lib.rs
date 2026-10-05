#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

#[cfg(feature = "callback")]
mod callback;
pub mod ext;
#[cfg(feature = "json")]
mod json;
#[cfg(feature = "callback")]
pub use callback::{Checkpoint, FnPulse};
pub mod poll;
mod pulse;
mod sync;
mod tree;

#[cfg(feature = "adapters")]
pub use how_far::WithStop;
// Application-facing protocol names only; implementor/checked helpers remain in how_far.
use how_far::ChildPulse;
pub use how_far::{
    Child, Complete, Execution, NoPulse, Outcome, Paced, PhaseSpec, Phases, PlanError, ProgressExt,
    Pulse, Report, ResultExt, SharedPulse, Stages, Stop, StopOnly, StopReason, Total, Unstoppable,
    prelude,
};

pub use pulse::PulseTree;
pub use tree::{NodeId, Observer, Phase, Reporter, Snapshot, Status, Summary};

//! Track, observe, and profile work that reports through `how-far`.
//!
//! Libraries depend only on the small `how-far` crate and accept
//! `&dyn Pulse`. Applications and tests depend on this crate and pass a
//! [`PulseTree`]: it records the phases, weights, counts and outcomes the
//! library declares, while an [`Observer`] reads snapshots from any thread.
//!
//! ```
//! use how_far_along::{Outcome, Phase, PhaseSpec, ProgressExt, Pulse, PulseTree, Stages, Total};
//! use almost_enough::Stopper;
//!
//! /// Library code: it depends only on `how-far`.
//! fn encode(rows: u64, pulse: &dyn Pulse) -> Result<(), how_far::RunError<how_far::StopReason>> {
//!     let mut stages = Stages::new(pulse, &[
//!         PhaseSpec::new("analyze", 1, Total::Exact(rows)),
//!         PhaseSpec::new("encode", 3, Total::Exact(rows)),
//!     ])?;
//!     for _ in 0..2 {
//!         stages.run_stoppable(|stage| {
//!             for _ in 0..rows {
//!                 stage.step(1)?;
//!             }
//!             Ok(())
//!         })?;
//!     }
//!     stages.finish()?;
//!     Ok(())
//! }
//!
//! // Application code.
//! let stop = Stopper::new(); // `stop.cancel()` from any thread stops the work.
//! let tree = PulseTree::new(Phase::new("job", Total::Unknown), stop.clone());
//! let observer = tree.observer();
//! let result = encode(100, &tree);
//! tree.finish(Outcome::from_result(&result, |_| true))?;
//! assert_eq!(observer.snapshot().fraction(), Some(1.0));
//! # Ok::<(), how_far_along::PlanError>(())
//! ```
//!
//! Applications can also plan a tree themselves with [`Phase`] and give
//! workers [`Reporter`]s. The [`poll`] module dispatches callbacks over
//! snapshots; `profile` and `diagnostics` (opt-in features) measure checkpoint
//! cadence. Everything in `how-far` is re-exported here.
#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

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

pub use how_far::{
    Child, ChildPulse, Execution, NoPulse, NoReport, Outcome, PhaseSpec, PlanError, ProgressExt,
    ProgressWithStop, Pulse, PulseHandle, Report, RunError, Stages, Stop, StopReason, Total,
    Unstoppable,
};
pub use pulse::PulseTree;
pub use tree::{NodeId, Observer, Phase, Reporter, Snapshot, Status};

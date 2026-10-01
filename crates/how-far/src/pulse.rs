//! Phase planning and the combined cancellation/progress interface.

use crate::Report;
use alloc::{boxed::Box, vec::Vec};
use enough::{Stop, StopReason};

/// The denominator for completed work in one phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Total {
    /// A known count. Exceeding it is visible to observers.
    Exact(u64),
    /// A revisable estimate; reaching it does not finish the phase.
    Estimated(u64),
    /// No useful denominator yet.
    Unknown,
}

/// How work in a phase (or its child phases) is scheduled; planning does not schedule it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Execution {
    /// No scheduling relationship is asserted.
    #[default]
    Unspecified,
    /// Children run in order.
    Sequence,
    /// Concurrent branches all join before the parent finishes.
    ForkJoin,
    /// Logical tasks share a pool with the configured ceiling.
    WorkPool {
        /// A positive configured concurrency ceiling.
        max_parallelism: usize,
    },
}

/// Explicit terminal result, independent of reaching a count total.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
    /// All required work completed successfully.
    Succeeded,
    /// This planned work proved unnecessary.
    Skipped,
    /// Work stopped cooperatively.
    Cancelled,
    /// Work failed for a domain-specific reason.
    Failed,
    /// The phase owner was dropped without a terminal result.
    Abandoned,
}

/// Invalid phase plan or lifecycle transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PlanError {
    /// A partition needs at least one child and positive weights.
    EmptyOrZeroWeight,
    /// The weight sum or job-local identifier space overflowed.
    Overflow,
    /// Planning must precede reporting and cannot be repeated.
    AlreadyInUse,
    /// The phase is already terminal.
    Finished,
    /// All children must finish before the parent does.
    UnfinishedChildren,
    /// Every declared child has already run.
    NoMoreChildren,
    /// A successful parent cannot contain unsuccessful children.
    UnsuccessfulChildren,
    /// A work pool needs at least one possible worker.
    ZeroParallelism,
    /// Another thread is administering the phase.
    Busy,
}
impl core::fmt::Display for PlanError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::EmptyOrZeroWeight => "a partition needs positive child weights",
            Self::Overflow => "phase weights or identifiers overflowed",
            Self::AlreadyInUse => "phase planning must precede use",
            Self::Finished => "the phase is already finished",
            Self::UnfinishedChildren => "join and finish every child first",
            Self::NoMoreChildren => "no declared child remains to run",
            Self::UnsuccessfulChildren => "a required child did not succeed",
            Self::ZeroParallelism => "pool parallelism must be positive",
            Self::Busy => "another thread is administering the phase",
        })
    }
}
impl core::error::Error for PlanError {}

/// Borrowed description of one child in a fixed partition.
///
/// Weights are relative to siblings, so `[35, 30, 35]` reserves exactly 30%
/// for the middle phase. Declare every sibling before starting any of them.
/// Construct this with [`Self::new`]; its fields remain readable, while future
/// planning options can be added without changing library authors' code.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct PhaseSpec<'a> {
    /// Human-readable phase name.
    pub name: &'a str,
    /// Positive relative weight among siblings.
    pub weight: u64,
    /// Denominator for this phase's completed units.
    pub total: Total,
    /// Name of a counted unit, such as `rows` or `superblocks`.
    pub units: &'a str,
    /// Scheduling of this phase's work, including shared workers or children.
    pub execution: Execution,
}
impl<'a> PhaseSpec<'a> {
    /// Describe one weighted child; the default unit is `items`.
    pub const fn new(name: &'a str, weight: u64, total: Total) -> Self {
        Self {
            name,
            weight,
            total,
            units: "items",
            execution: Execution::Unspecified,
        }
    }
    /// Name the counted unit.
    pub const fn units(mut self, units: &'a str) -> Self {
        self.units = units;
        self
    }
    /// Describe this phase's scheduling, including workers sharing one phase.
    pub const fn execution(mut self, execution: Execution) -> Self {
        self.execution = execution;
        self
    }
}

/// One callback for cancellation, completed work, and nested phase planning.
///
/// Libraries depend only on `how_far` and accept `&dyn Pulse`. The caller owns
/// observation and decides whether to use the optional `how-far-along` tracker.
/// `split` runs at a phase boundary; `advance` and `check` remain separate hot
/// path operations. Call `finish` after children have joined.
pub trait Pulse: Stop + Report {
    /// Declare every child before reporting into this phase. Each returned
    /// child can itself be passed to an algorithm as `&dyn Pulse`.
    fn split(
        &self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Box<dyn Pulse + '_>>, PlanError>;

    /// Publish the phase's terminal outcome after all child work has joined.
    fn finish(&self, outcome: Outcome) -> Result<(), PlanError>;
}

/// Ignore progress and never cancel, including in nested phases.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoPulse;
impl Stop for NoPulse {
    #[inline(always)]
    fn check(&self) -> Result<(), StopReason> {
        Ok(())
    }
    #[inline(always)]
    fn may_stop(&self) -> bool {
        false
    }
}
impl Report for NoPulse {
    #[inline(always)]
    fn advance(&self, _: u64) {}
    #[inline(always)]
    fn may_report(&self) -> bool {
        false
    }
}
impl Pulse for NoPulse {
    fn split(
        &self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Box<dyn Pulse + '_>>, PlanError> {
        if matches!(execution, Execution::WorkPool { max_parallelism: 0 }) {
            return Err(PlanError::ZeroParallelism);
        }
        if parts.is_empty() || parts.iter().any(|part| part.weight == 0) {
            return Err(PlanError::EmptyOrZeroWeight);
        }
        if parts
            .iter()
            .any(|part| matches!(part.execution, Execution::WorkPool { max_parallelism: 0 }))
        {
            return Err(PlanError::ZeroParallelism);
        }
        parts
            .iter()
            .try_fold(0_u64, |sum, part| sum.checked_add(part.weight))
            .ok_or(PlanError::Overflow)?;
        Ok(parts
            .iter()
            .map(|_| Box::new(Self) as Box<dyn Pulse>)
            .collect())
    }
    #[inline(always)]
    fn finish(&self, _: Outcome) -> Result<(), PlanError> {
        Ok(())
    }
}

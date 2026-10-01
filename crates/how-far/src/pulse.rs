//! Phase planning and the combined cancellation/progress interface.

use crate::{ProgressWithStop, Report};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{fmt, num::NonZeroUsize};
use enough::{Stop, StopReason};

/// The denominator for completed work in one phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Total {
    /// A known count. Counting past it is recorded as an overrun.
    Exact(u64),
    /// A revisable estimate. Reaching it does not finish the phase.
    Estimated(u64),
    /// No useful denominator yet.
    Unknown,
}

/// How a phase's work is scheduled. Declaring it schedules nothing.
///
/// On a leaf, it describes the leaf's own units: `WorkPool` means several
/// workers share one count. On a branch, it describes how the children run.
/// When a phase splits, the execution passed to [`Pulse::split`] replaces the
/// one in its [`PhaseSpec`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Execution {
    /// No scheduling relationship is declared.
    #[default]
    Unspecified,
    /// One after another, in declared order.
    Sequence,
    /// Concurrently; all join before the parent finishes.
    ForkJoin,
    /// Shared by a pool of at most `max_parallelism` workers.
    /// Build it with [`Execution::work_pool`].
    #[non_exhaustive]
    WorkPool {
        /// The pool's concurrency ceiling, not a count of dedicated cores.
        max_parallelism: NonZeroUsize,
    },
}

impl Execution {
    /// Work shared by a pool of at most `max_parallelism` workers, such as
    /// `std::thread::available_parallelism()` or a Rayon pool's thread count.
    pub const fn work_pool(max_parallelism: NonZeroUsize) -> Self {
        Self::WorkPool { max_parallelism }
    }
}

/// How a phase ended. Reaching a count total is not an outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Outcome {
    /// All of the phase's work completed.
    Succeeded,
    /// The planned work turned out to be unnecessary.
    Skipped,
    /// Work stopped cooperatively, after a cancellation request or timeout.
    Cancelled,
    /// Work failed for a reason other than a stop request.
    Failed,
    /// The phase's owner went away without publishing an outcome.
    Abandoned,
}

impl Outcome {
    /// The outcome of an operation's result: `Succeeded` for `Ok`, `Cancelled`
    /// for an error `is_stop` accepts, and `Failed` for any other error.
    ///
    /// ```
    /// use how_far::{Outcome, StopReason};
    ///
    /// let stopped: Result<(), StopReason> = Err(StopReason::Cancelled);
    /// assert_eq!(Outcome::from_result(&stopped, |_| true), Outcome::Cancelled);
    /// ```
    pub fn from_result<T, E>(result: &Result<T, E>, is_stop: impl FnOnce(&E) -> bool) -> Self {
        match result {
            Ok(_) => Self::Succeeded,
            Err(error) if is_stop(error) => Self::Cancelled,
            Err(_) => Self::Failed,
        }
    }
}

/// A phase plan that cannot be applied, or a lifecycle step out of order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PlanError {
    /// A split needs at least one part, and every weight must be positive.
    EmptyOrZeroWeight,
    /// The weights' sum or the job's phase identifiers overflowed.
    Overflow,
    /// The phase already counted work or planned children. Plan once, first.
    AlreadyInUse,
    /// The phase has children; only a leaf has its own total.
    NotALeaf,
    /// This pulse cannot plan children.
    Unsupported,
    /// The phase already has an outcome.
    Finished,
    /// Every child must finish before its parent.
    UnfinishedChildren,
    /// A phase cannot succeed while one of its children did not.
    UnsuccessfulChildren,
    /// Every declared stage has already run.
    NoMoreStages,
    /// Another thread is planning or finishing this phase.
    Busy,
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EmptyOrZeroWeight => "a split needs at least one part with a positive weight",
            Self::Overflow => "phase weights or identifiers overflowed",
            Self::AlreadyInUse => "the phase already counted work or planned children",
            Self::NotALeaf => "only a leaf phase has its own total",
            Self::Unsupported => "this pulse cannot plan children",
            Self::Finished => "the phase already has an outcome",
            Self::UnfinishedChildren => "every child must finish before its parent",
            Self::UnsuccessfulChildren => "a phase cannot succeed while a child did not",
            Self::NoMoreStages => "every declared stage has already run",
            Self::Busy => "another thread is planning or finishing this phase",
        })
    }
}

impl core::error::Error for PlanError {}

/// One child in a split: a name, a weight relative to its siblings, a total,
/// and optionally a unit name and an execution model.
///
/// Weights are relative, so `[35, 30, 35]` gives the middle child exactly 30%
/// of its parent however many workers it later uses. Build it with
/// [`PhaseSpec::new`]; the fields stay readable, and new planning options can
/// be added without breaking existing code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PhaseSpec<'a> {
    /// Human-readable phase name.
    pub name: &'a str,
    /// Positive weight relative to the siblings.
    pub weight: u64,
    /// Denominator for this phase's completed units.
    pub total: Total,
    /// Name of the counted unit, such as `rows` or `superblocks`.
    pub units: &'a str,
    /// How this phase's work is scheduled.
    pub execution: Execution,
}

impl<'a> PhaseSpec<'a> {
    /// Describe one weighted child that counts `items`.
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

    /// Declare how this phase's work is scheduled.
    pub const fn execution(mut self, execution: Execution) -> Self {
        self.execution = execution;
        self
    }
}

/// Check a split's parts the way every `Pulse` does.
pub(crate) fn validate(parts: &[PhaseSpec<'_>]) -> Result<(), PlanError> {
    if parts.is_empty() || parts.iter().any(|part| part.weight == 0) {
        return Err(PlanError::EmptyOrZeroWeight);
    }
    parts
        .iter()
        .try_fold(0_u64, |sum, part| sum.checked_add(part.weight))
        .map(|_| ())
        .ok_or(PlanError::Overflow)
}

/// Cancellation, completed work, and nested phase planning, in one value.
///
/// A library accepts `&dyn Pulse` and uses it three ways:
///
/// - `check()` asks whether to stop. It is the cheapest call; use it freely.
/// - `advance(n)` counts finished units; [`ProgressExt::step`](crate::ProgressExt::step)
///   counts and then checks.
/// - [`split`](Self::split) declares weighted children. The library owns the
///   returned [`Child`] handles and finishes each one. It never finishes the
///   pulse it was given; that pulse belongs to its caller.
///
/// Splitting and counting are exclusive: a phase either counts its own units
/// (a leaf) or delegates to children (a branch). Plan before the first report.
///
/// Wrappers that add measurement or forwarding implement this trait, so it is
/// open. Methods added in later compatible releases will have default bodies.
pub trait Pulse: Stop + Report {
    /// Split this phase into weighted children, in declared order.
    ///
    /// Returns exactly one [`Child`] per part. The caller owns them: pass
    /// `&child` to code that does the child's work, then finish the child once
    /// that work has joined. Fails if the parts are invalid, if this phase
    /// already counted work or split, or if the pulse cannot plan children.
    fn split(
        &self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Child<'_>>, PlanError>;

    /// An owned handle to this pulse's stop policy and counter.
    ///
    /// The handle checks the same stop policy and counts into the same phase,
    /// but it cannot plan or finish anything. It is `'static`, cloneable and
    /// thread-safe: give it to work that must own its stop policy or progress
    /// sink, such as a codec context that stores `impl Stop + 'static`, a
    /// `std::thread::spawn` worker, or an async task. Its `stop` field alone
    /// also implements `Stop`.
    ///
    /// A pulse that cannot provide a handle returns
    /// [`PulseHandle::default()`], which never stops and discards reports;
    /// such a pulse should document that.
    fn handle(&self) -> PulseHandle;
}

/// What a pulse's children add: publishing a terminal outcome.
///
/// Implement this for the type your [`Pulse::split`] returns, and wrap each
/// child with [`Child::new`]. Library code never calls it directly; it calls
/// [`Child::finish`], which consumes the handle so a child is finished at most
/// once.
pub trait ChildPulse: Pulse {
    /// Publish this child's terminal outcome.
    fn finish(self: Box<Self>, outcome: Outcome) -> Result<(), PlanError>;
}

/// A planned child phase, owned by the code that called [`Pulse::split`].
///
/// A `Child` is a [`Pulse`]: pass `&child` wherever `&dyn Pulse` is expected,
/// or call `check`, `step` and `split` on it directly. Finish it once, after
/// its work has joined. A tracker records `Outcome::Abandoned` for a child that
/// is dropped unfinished, including on unwinding. It is two words wide.
pub struct Child<'a> {
    pulse: Box<dyn ChildPulse + 'a>,
}

impl<'a> Child<'a> {
    /// Wrap one child returned from a [`Pulse::split`] implementation.
    pub fn new(pulse: impl ChildPulse + 'a) -> Self {
        Self {
            pulse: Box::new(pulse),
        }
    }

    /// Publish this child's outcome. Its parent can finish only after every
    /// child has finished.
    ///
    /// Fails without recording `outcome` if the child's own children are still
    /// running, or if `outcome` is `Succeeded` or `Skipped` while one of them
    /// did not succeed; a tracker then records the child as abandoned.
    pub fn finish(self, outcome: Outcome) -> Result<(), PlanError> {
        self.pulse.finish(outcome)
    }
}

impl fmt::Debug for Child<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Child").finish_non_exhaustive()
    }
}

impl Stop for Child<'_> {
    #[inline]
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.pulse.check()
    }
    #[inline]
    fn may_stop(&self) -> bool {
        self.pulse.may_stop()
    }
}

impl Report for Child<'_> {
    #[inline]
    #[track_caller]
    fn advance(&self, completed: u64) {
        self.pulse.advance(completed);
    }
    #[inline]
    fn may_report(&self) -> bool {
        self.pulse.may_report()
    }
}

impl Pulse for Child<'_> {
    fn split(
        &self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Child<'_>>, PlanError> {
        self.pulse.split(execution, parts)
    }
    fn handle(&self) -> PulseHandle {
        self.pulse.handle()
    }
}

/// An owned, cloneable, `'static` view of a pulse's stop policy and counter.
///
/// Returned by [`Pulse::handle`]. `stop` and `report` are `None` when the
/// source pulse never stops or discards reports. The handle implements `Stop`
/// and `Report`, so `check`, `advance` and `step` work on it directly.
pub type PulseHandle = ProgressWithStop<Option<Arc<dyn Stop>>, Option<Arc<dyn Report>>>;

/// Never stops and discards reports, including in every nested phase.
///
/// It still validates plans, so a library's planning mistakes surface even
/// when nobody observes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
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
    fn split(&self, _: Execution, parts: &[PhaseSpec<'_>]) -> Result<Vec<Child<'_>>, PlanError> {
        validate(parts)?;
        Ok(parts.iter().map(|_| Child::new(Self)).collect())
    }
    fn handle(&self) -> PulseHandle {
        PulseHandle::default()
    }
}

impl ChildPulse for NoPulse {
    #[inline]
    fn finish(self: Box<Self>, _: Outcome) -> Result<(), PlanError> {
        Ok(())
    }
}

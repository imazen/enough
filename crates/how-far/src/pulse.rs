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
    /// Prevented from running by an earlier error; earns no completed share.
    NotRun,
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
    /// This borrowed implementation cannot provide an owned pulse.
    NotShareable,
    /// The phase already has an outcome.
    Finished,
    /// Every child must finish before its parent.
    UnfinishedChildren,
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
            Self::NotShareable => "this pulse cannot provide an owned handle",
            Self::Finished => "the phase already has an outcome",
            Self::UnfinishedChildren => "every child must finish before its parent",
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

    /// Check a split's parts the way every [`Pulse::split`] must, and return
    /// the sum of their weights.
    ///
    /// Fails with [`PlanError::EmptyOrZeroWeight`] for no parts or a zero
    /// weight, and [`PlanError::Overflow`] if the weights' sum overflows. Call
    /// it first in a `split` implementation, so every pulse rejects the same
    /// plans with the same errors.
    pub fn validate_split(parts: &[PhaseSpec<'_>]) -> Result<u64, PlanError> {
        // Plain loops keep this crate's compiled code small.
        if parts.is_empty() {
            return Err(PlanError::EmptyOrZeroWeight);
        }
        let mut sum = 0_u64;
        for part in parts {
            if part.weight == 0 {
                return Err(PlanError::EmptyOrZeroWeight);
            }
            sum = sum.checked_add(part.weight).ok_or(PlanError::Overflow)?;
        }
        Ok(sum)
    }
}

/// Cancellation, completed work, and nested phase planning, in one value.
///
/// A library accepts `&dyn Pulse` and uses it three ways:
///
/// - `check()` asks whether to stop. It is the cheapest call, unless the
///   caller made it run code of its own, as a checkpoint
///   callback does; check once per row, block or tile, not per byte.
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
///
/// # Implementing
///
/// A tracker or checkpoint callback adapter from `how-far-along` usually
/// suffices. Implementations must keep these rules:
///
/// - `check` and `advance` may run on many threads at once, and `advance` must
///   not block. `may_stop` and `may_report` return `false` only when that can
///   never change.
/// - `split` starts with [`PhaseSpec::validate_split`]. It fails with
///   [`PlanError::AlreadyInUse`] if this phase already counted work or split,
///   and otherwise returns one [`Child::new`] per part, in order, each
///   checking and counting like any pulse. Children from [`Child::inert`]
///   cannot be stopped or observed, so they suit only parts nobody watches.
/// - `handle` returns an owned handle that checks the same stop and counts
///   into the same phase, or [`PulseHandle::default()`], documented as such.
/// - [`ChildPulse::finish`] fails with [`PlanError::UnfinishedChildren`] while
///   a descendant is unfinished. Completed failed attempts may belong to a
///   successful parent. A dropped unfinished child records `Abandoned`.
pub trait Pulse: Stop + Report {
    /// Record a reporting-protocol problem for optional diagnostics. This never
    /// changes cancellation or the operation's return value. Ordinary sinks may
    /// ignore it; diagnostic wrappers retain bounded evidence.
    #[track_caller]
    fn record_issue(&self, _error: PlanError) {}
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

    /// Mark this phase running, without counting or ending planning.
    /// Implementations without lifecycle observations may ignore this.
    fn start(&self) -> Result<(), PlanError> {
        Ok(())
    }

    /// Revise a leaf's denominator without changing completed units or weights.
    /// Unsupported implementations return an error instead of losing the update.
    fn set_total(&self, _total: Total) -> Result<(), PlanError> {
        Err(PlanError::Unsupported)
    }

    /// Share all pulse capabilities with owned work, but never completion rights.
    /// Join such work before finishing the original owner. Unsupported ownership
    /// is explicit; this method never silently replaces cancellation with a no-op.
    fn share(&self) -> Result<SharedPulse, PlanError> {
        Err(PlanError::NotShareable)
    }
}

// A reference, box or `Arc` of a pulse is a pulse, as for `Stop` and `Report`,
// so generic and owned code can hold any pulse. Checkpoints recognize only
// `&NoPulse` itself, so `&&NoPulse` works but pays for its calls.

impl<P: Pulse + ?Sized> Pulse for &P {
    #[track_caller]
    fn record_issue(&self, error: PlanError) {
        (**self).record_issue(error);
    }
    #[inline]
    fn split(
        &self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Child<'_>>, PlanError> {
        (**self).split(execution, parts)
    }
    #[inline]
    fn handle(&self) -> PulseHandle {
        (**self).handle()
    }
    fn start(&self) -> Result<(), PlanError> {
        (**self).start()
    }
    fn set_total(&self, total: Total) -> Result<(), PlanError> {
        (**self).set_total(total)
    }
    fn share(&self) -> Result<SharedPulse, PlanError> {
        (**self).share()
    }
}

impl<P: Pulse + ?Sized> Pulse for &mut P {
    #[track_caller]
    fn record_issue(&self, error: PlanError) {
        (**self).record_issue(error);
    }
    #[inline]
    fn split(
        &self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Child<'_>>, PlanError> {
        (**self).split(execution, parts)
    }
    #[inline]
    fn handle(&self) -> PulseHandle {
        (**self).handle()
    }
    fn start(&self) -> Result<(), PlanError> {
        (**self).start()
    }
    fn set_total(&self, total: Total) -> Result<(), PlanError> {
        (**self).set_total(total)
    }
    fn share(&self) -> Result<SharedPulse, PlanError> {
        (**self).share()
    }
}

impl<P: Pulse + ?Sized> Pulse for Box<P> {
    #[track_caller]
    fn record_issue(&self, error: PlanError) {
        (**self).record_issue(error);
    }
    #[inline]
    fn split(
        &self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Child<'_>>, PlanError> {
        (**self).split(execution, parts)
    }
    #[inline]
    fn handle(&self) -> PulseHandle {
        (**self).handle()
    }
    fn start(&self) -> Result<(), PlanError> {
        (**self).start()
    }
    fn set_total(&self, total: Total) -> Result<(), PlanError> {
        (**self).set_total(total)
    }
    fn share(&self) -> Result<SharedPulse, PlanError> {
        (**self).share()
    }
}

impl<P: Pulse + ?Sized> Pulse for Arc<P> {
    #[track_caller]
    fn record_issue(&self, error: PlanError) {
        (**self).record_issue(error);
    }
    #[inline]
    fn split(
        &self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Child<'_>>, PlanError> {
        (**self).split(execution, parts)
    }
    #[inline]
    fn handle(&self) -> PulseHandle {
        (**self).handle()
    }
    fn start(&self) -> Result<(), PlanError> {
        (**self).start()
    }
    fn set_total(&self, total: Total) -> Result<(), PlanError> {
        (**self).set_total(total)
    }
    fn share(&self) -> Result<SharedPulse, PlanError> {
        (**self).share()
    }
}

/// What a pulse's children add: publishing a terminal outcome.
///
/// Implement this for the type your [`Pulse::split`] returns, and wrap each
/// child with [`Child::new`]. Library code never calls it directly; it calls
/// [`Child::finish`], which consumes the handle so a child is finished at most
/// once.
pub trait ChildPulse: Pulse {
    /// Resolve an untouched phase from its enclosing result. The default uses
    /// ordinary completion; trackers can retain the inference as evidence.
    fn complete_inferred(self: Box<Self>, outcome: Outcome) {
        self.complete_as(outcome);
    }
    /// Complete best-effort observation. Trackers may infer untouched descendants
    /// from this result; the default preserves older implementors' finish logic.
    fn complete_as(self: Box<Self>, outcome: Outcome) {
        let _ = self.finish(outcome);
    }
    /// Publish this child's terminal outcome.
    fn finish(self: Box<Self>, outcome: Outcome) -> Result<(), PlanError>;
}

/// A planned child phase, owned by the code that called [`Pulse::split`].
///
/// A `Child` is a [`Pulse`]: pass `&child` wherever `&dyn Pulse` is expected,
/// or call `check`, `step` and `split` on it directly. Finish it once, after
/// its work has joined. A tracker records `Outcome::Abandoned` for a child that
/// is dropped unfinished, including on unwinding. It is two words wide.
/// Completion consumes the owner, so it cannot be repeated:
///
/// ```compile_fail,E0382
/// use how_far::{Child, Outcome};
/// let child = Child::inert();
/// child.finish(Outcome::Succeeded).unwrap();
/// child.finish(Outcome::Succeeded).unwrap();
/// ```
#[must_use = "finish the child after its work joins; dropping a tracked child abandons it"]
pub struct Child<'a> {
    /// `None` for a child of [`NoPulse`], which needs no allocation.
    pulse: Option<Box<dyn ChildPulse + 'a>>,
}

impl<'a> Child<'a> {
    /// Resolve an unused phase from its enclosing result, retaining inference evidence.
    pub fn complete_inferred(self, outcome: Outcome) {
        if let Some(pulse) = self.pulse {
            pulse.complete_inferred(outcome);
        }
    }
    /// A cancellation/checkpoint-preserving view without progress accounting.
    pub(crate) fn untracked(parent: &'a dyn Pulse) -> Self {
        Self::new(Untracked { inner: parent })
    }

    /// Plan nested observations without introducing a new work error.
    #[track_caller]
    pub fn plan(&self, execution: Execution, parts: &[PhaseSpec<'_>]) -> Vec<Child<'_>> {
        self.pulse().plan(execution, parts)
    }
    /// Wrap one child returned from a [`Pulse::split`] implementation.
    pub fn new(pulse: impl ChildPulse + 'a) -> Self {
        Self {
            pulse: Some(Box::new(pulse)),
        }
    }

    /// A child like those [`NoPulse`] hands out: it never stops, discards
    /// reports, and finishing it does nothing. Its work cannot be stopped
    /// through it and nobody sees its progress, so return it from
    /// [`Pulse::split`] only for parts that nobody needs to observe or stop.
    pub const fn inert() -> Self {
        Self { pulse: None }
    }

    /// Publish this child's outcome. Its parent can finish only after every
    /// child has finished.
    ///
    /// Fails without recording `outcome` if the child's own children are still
    /// running; the consumed child is then dropped, so a tracker records it as
    /// abandoned. Finished children's outcomes need not match: a successful
    /// child may retain a failed attempt it recovered from.
    pub fn finish(self, outcome: Outcome) -> Result<(), PlanError> {
        match self.pulse {
            Some(pulse) => pulse.finish(outcome),
            None => Ok(()),
        }
    }

    /// The child's own pulse, without the forwarding a `Child` adds when it is
    /// itself used as a `&dyn Pulse`: the static [`NoPulse`] for its children.
    #[inline]
    pub(crate) fn pulse(&self) -> &(dyn Pulse + 'a) {
        match &self.pulse {
            Some(pulse) => &**pulse,
            None => &NoPulse,
        }
    }
}

impl fmt::Debug for Child<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Child").finish_non_exhaustive()
    }
}

impl crate::Complete for Child<'_> {
    fn complete_as(self, outcome: Outcome) {
        if let Some(pulse) = self.pulse {
            pulse.complete_as(outcome);
        }
    }
}

impl dyn Pulse + '_ {
    /// Best-effort planning. If the sink rejects the plan, returns one untracked
    /// child per part, all retaining this pulse's stop/checkpoint policy.
    #[track_caller]
    pub fn plan(&self, execution: Execution, parts: &[PhaseSpec<'_>]) -> Vec<Child<'_>> {
        match self.split(execution, parts) {
            Ok(children) if children.len() == parts.len() => children,
            Ok(_) => {
                self.record_issue(PlanError::Unsupported);
                parts.iter().map(|_| Child::untracked(self)).collect()
            }
            Err(error) => {
                self.record_issue(error);
                parts.iter().map(|_| Child::untracked(self)).collect()
            }
        }
    }
}

struct Untracked<P> {
    inner: P,
}
impl<P: Pulse> Stop for Untracked<P> {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.inner.check()
    }
    fn may_stop(&self) -> bool {
        self.inner.may_stop()
    }
}
impl<P: Pulse> Report for Untracked<P> {
    fn advance(&self, _: u64) {}
    fn may_report(&self) -> bool {
        false
    }
}
impl<P: Pulse> Pulse for Untracked<P> {
    fn split(&self, _: Execution, parts: &[PhaseSpec<'_>]) -> Result<Vec<Child<'_>>, PlanError> {
        PhaseSpec::validate_split(parts)?;
        Ok(parts
            .iter()
            .map(|_| Child::untracked(&self.inner))
            .collect())
    }
    fn handle(&self) -> PulseHandle {
        let handle = self.inner.handle();
        ProgressWithStop::new(handle.stop, None)
    }
    fn share(&self) -> Result<SharedPulse, PlanError> {
        Ok(SharedPulse::new(Untracked {
            inner: self.inner.share()?,
        }))
    }
    #[track_caller]
    fn record_issue(&self, error: PlanError) {
        self.inner.record_issue(error);
    }
}
impl<P: Pulse> ChildPulse for Untracked<P> {
    fn finish(self: Box<Self>, _: Outcome) -> Result<(), PlanError> {
        Ok(())
    }
}

impl Stop for Child<'_> {
    #[inline]
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        match &self.pulse {
            Some(pulse) => pulse.check(),
            None => Ok(()),
        }
    }
    #[inline]
    fn may_stop(&self) -> bool {
        matches!(&self.pulse, Some(pulse) if pulse.may_stop())
    }
}

impl Report for Child<'_> {
    #[inline]
    #[track_caller]
    fn advance(&self, completed: u64) {
        if let Some(pulse) = &self.pulse {
            pulse.advance(completed);
        }
    }
    #[inline]
    fn may_report(&self) -> bool {
        matches!(&self.pulse, Some(pulse) if pulse.may_report())
    }
}

impl Pulse for Child<'_> {
    #[track_caller]
    fn record_issue(&self, error: PlanError) {
        self.pulse().record_issue(error);
    }
    fn split(
        &self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Child<'_>>, PlanError> {
        self.pulse().split(execution, parts)
    }
    fn handle(&self) -> PulseHandle {
        self.pulse().handle()
    }
    fn start(&self) -> Result<(), PlanError> {
        self.pulse().start()
    }
    fn set_total(&self, total: Total) -> Result<(), PlanError> {
        self.pulse().set_total(total)
    }
    fn share(&self) -> Result<SharedPulse, PlanError> {
        self.pulse().share()
    }
}

/// An owned, cloneable, `'static` view of a pulse's stop policy and counter.
///
/// Returned by [`Pulse::handle`]. `stop` and `report` are `None` when the
/// source pulse never stops or discards reports. The handle implements `Stop`
/// and `Report`, so `check`, `advance` and `step` work on it directly.
pub type PulseHandle = ProgressWithStop<Option<Arc<dyn Stop>>, Option<Arc<dyn Report>>>;

/// The type of [`NoPulse`]. It has exactly one value, that static: nothing
/// outside this crate can build, copy or move one.
pub struct Inert {
    /// One byte, so that the static has an address of its own.
    _unique: u8,
}

/// Never stops and discards reports, including in every nested phase.
///
/// Pass `&NoPulse`. [`step`](crate::ProgressExt::step),
/// [`live`](crate::ProgressExt::live) and [`Paced`](crate::Paced) recognize it
/// by its address, so a loop that steps on every iteration runs with no
/// checkpoint code when nobody listens: the one comparison moves out of the
/// loop. Where it cannot, it is a comparison and a branch. A bare `check()` or
/// `advance()` through a `&dyn Pulse` still makes one call, to a function that
/// returns at once. A pulse that wraps it, such as `&&NoPulse` or a
/// `Box<&NoPulse>`, works but is not recognized, and pays for both calls.
///
/// Its children and stages are free the same way and need no allocation. It
/// still validates plans, so a library's planning mistakes surface even when
/// nobody observes it.
#[allow(non_upper_case_globals)]
pub static NoPulse: Inert = Inert { _unique: 0 };

impl fmt::Debug for Inert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NoPulse")
    }
}

impl Stop for Inert {
    #[inline(always)]
    fn check(&self) -> Result<(), StopReason> {
        Ok(())
    }
    #[inline(always)]
    fn may_stop(&self) -> bool {
        false
    }
}

impl Report for Inert {
    #[inline(always)]
    fn advance(&self, _: u64) {}
    #[inline(always)]
    fn may_report(&self) -> bool {
        false
    }
}

impl Pulse for Inert {
    fn split(&self, _: Execution, parts: &[PhaseSpec<'_>]) -> Result<Vec<Child<'_>>, PlanError> {
        PhaseSpec::validate_split(parts)?;
        let mut children = Vec::with_capacity(parts.len());
        for _ in parts {
            children.push(Child::inert());
        }
        Ok(children)
    }
    fn handle(&self) -> PulseHandle {
        ProgressWithStop::new(None, None)
    }
    fn set_total(&self, _: Total) -> Result<(), PlanError> {
        Ok(())
    }
    fn share(&self) -> Result<SharedPulse, PlanError> {
        Ok(SharedPulse::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProgressExt;
    use alloc::format;

    #[test]
    fn no_pulse_children_are_the_static_and_need_no_allocation() {
        let parts = [
            PhaseSpec::new("a", 1, Total::Exact(1)),
            PhaseSpec::new("b", 1, Total::Exact(1)),
        ];
        let children = NoPulse.split(Execution::Sequence, &parts).unwrap();
        for child in children {
            assert!(child.pulse.is_none(), "no boxed pulse");
            assert!(core::ptr::addr_eq(child.pulse(), &NoPulse));
            assert!(!child.may_stop() && !child.may_report());
            assert!(child.live().is_none());
            assert_eq!(child.step(5), Ok(()));
            assert_eq!(
                format!("{:?}", child.paced(1)),
                "Paced { live: false, pending: 0, every: 1 }"
            );
            child.finish(Outcome::Succeeded).unwrap();
        }
        assert_eq!(
            NoPulse.split(Execution::Sequence, &[]).err(),
            Some(PlanError::EmptyOrZeroWeight)
        );
    }
}

/// An owned, cloneable pulse that retains planning and cancellation capabilities.
/// The original phase owner alone may finish it. Requires pointer atomics.
///
/// A shared view has no completion operation:
///
/// ```compile_fail,E0599
/// use how_far::{Outcome, SharedPulse};
/// let shared = SharedPulse::default();
/// shared.finish(Outcome::Succeeded).unwrap();
/// ```
#[derive(Clone, Default)]
pub struct SharedPulse(Option<Arc<dyn Pulse>>);
impl SharedPulse {
    /// Erase an owned implementation. The implementation must not own the
    /// phase's completion token: dropping clones must not finish the phase.
    pub fn new(pulse: impl Pulse + 'static) -> Self {
        Self(Some(Arc::new(pulse)))
    }
    /// Borrow the underlying pulse, preserving the canonical no-op fast path.
    pub fn as_pulse(&self) -> &dyn Pulse {
        match &self.0 {
            Some(p) => &**p,
            None => &NoPulse,
        }
    }
}
impl fmt::Debug for SharedPulse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedPulse")
            .field("live", &self.0.is_some())
            .finish()
    }
}
impl Stop for SharedPulse {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.as_pulse().check()
    }
    fn may_stop(&self) -> bool {
        self.as_pulse().may_stop()
    }
}
impl Report for SharedPulse {
    #[track_caller]
    fn advance(&self, n: u64) {
        self.as_pulse().advance(n)
    }
    fn may_report(&self) -> bool {
        self.as_pulse().may_report()
    }
}
impl Pulse for SharedPulse {
    #[track_caller]
    fn record_issue(&self, error: PlanError) {
        self.as_pulse().record_issue(error);
    }
    fn split(
        &self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Child<'_>>, PlanError> {
        self.as_pulse().split(execution, parts)
    }
    fn handle(&self) -> PulseHandle {
        self.as_pulse().handle()
    }
    fn start(&self) -> Result<(), PlanError> {
        self.as_pulse().start()
    }
    fn set_total(&self, total: Total) -> Result<(), PlanError> {
        self.as_pulse().set_total(total)
    }
    fn share(&self) -> Result<SharedPulse, PlanError> {
        Ok(self.clone())
    }
}

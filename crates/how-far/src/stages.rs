//! Running a sequential plan.

use crate::{Child, Execution, Outcome, PhaseSpec, PlanError, Pulse};
use alloc::collections::VecDeque;
use core::fmt;

/// An operation error, or an error applying the progress plan.
///
/// Libraries usually convert it into their own error type with `From`.
/// Match the variants you handle and keep a wildcard arm.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RunError<E> {
    /// The operation failed or stopped.
    Work(E),
    /// The phase plan could not be applied.
    Plan(PlanError),
}

impl<E> From<PlanError> for RunError<E> {
    fn from(error: PlanError) -> Self {
        Self::Plan(error)
    }
}

impl<E: fmt::Display> fmt::Display for RunError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Work(error) => error.fmt(f),
            Self::Plan(error) => error.fmt(f),
        }
    }
}

impl<E: core::error::Error + 'static> core::error::Error for RunError<E> {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Work(error) => Some(error),
            Self::Plan(error) => Some(error),
        }
    }
}

impl<E> RunError<RunError<E>> {
    fn flatten(self) -> RunError<E> {
        match self {
            Self::Work(error) => error,
            Self::Plan(error) => RunError::Plan(error),
        }
    }
}

/// Run a sequential plan one stage at a time.
///
/// [`Stages::new`] splits a pulse into the declared stages. Each `run` method
/// hands its closure the next stage as `&dyn Pulse`, then finishes that stage:
/// `Succeeded` if the closure returned `Ok`, otherwise the outcome its error
/// maps to. After an error, every later stage is finished as `NotRun`, and
/// the original error is returned unchanged. If the closure panics, the stage
/// and every later one are recorded as abandoned when `Stages` is dropped. A
/// caller that catches the panic and calls a `run` method again gets
/// `PlanError::Finished`: that stage is recorded as abandoned and the rest as
/// skipped.
///
/// `Stages` never finishes the pulse it was given. That pulse belongs to the
/// caller, which finishes it: an application finishes the root, and an outer
/// `Stages` finishes the stage it handed to this code. That is what lets one
/// library call another inside a stage.
///
/// ```
/// use how_far::{PhaseSpec, ProgressExt, Pulse, RunError, Stages, StopReason, Total};
///
/// /// A library that a caller can run inside one of its own stages.
/// fn resize(rows: u64, pulse: &dyn Pulse) -> Result<(), RunError<StopReason>> {
///     let mut stages = Stages::new(pulse, &[
///         PhaseSpec::new("horizontal", 1, Total::Exact(rows)),
///         PhaseSpec::new("vertical", 1, Total::Exact(rows)),
///     ])?;
///     for _ in 0..2 {
///         stages.run_stoppable(|stage| {
///             for _ in 0..rows {
///                 stage.step(1)?;
///             }
///             Ok(())
///         })?;
///     }
///     stages.finish()?;
///     Ok(())
/// }
///
/// fn thumbnail(pulse: &dyn Pulse) -> Result<(), RunError<StopReason>> {
///     let mut stages = Stages::new(pulse, &[
///         PhaseSpec::new("decode", 1, Total::Exact(1)),
///         PhaseSpec::new("resize", 4, Total::Unknown),
///     ])?;
///     stages.run_stoppable(|stage| stage.step(1))?;
///     // `resize` splits this stage; `Stages` finishes it afterwards.
///     stages.run_nested(|_| true, |stage| resize(8, stage))?;
///     stages.finish()?;
///     Ok(())
/// }
///
/// thumbnail(&how_far::NoPulse)?;
/// # Ok::<(), RunError<StopReason>>(())
/// ```
///
/// Workers that count one logical stage can share its `&dyn Pulse`; join them
/// before the closure returns. When workers need their own totals or outcomes,
/// split the stage and use [`run_nested`](Self::run_nested).
pub struct Stages<'a> {
    /// The stages that have not finished, in declared order.
    pending: VecDeque<Child<'a>>,
    /// The front stage was handed to work that has not returned.
    running: bool,
    stopped: bool,
}

impl fmt::Debug for Stages<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Stages")
            .field("remaining", &self.pending.len())
            .field("stopped", &self.stopped)
            .finish()
    }
}

// Each `run` method is instantiated once per closure, in the caller's crate,
// so it only fetches the stage, calls the closure and hands the result on.
// The bookkeeping is in `next_stage` and `end_stage`, which are compiled once
// here, and in `complete`, which is shared by calls with the same types.
impl<'a> Stages<'a> {
    /// Split `parent` into sequential stages, before any of their work starts.
    pub fn new(parent: &'a dyn Pulse, parts: &[PhaseSpec<'_>]) -> Result<Self, PlanError> {
        Ok(Self {
            pending: parent.split(Execution::Sequence, parts)?.into(),
            running: false,
            stopped: false,
        })
    }

    /// Discharge an intentionally unnecessary stage, without running it.
    pub fn skip(&mut self) -> Result<(), PlanError> {
        self.next_stage()?;
        self.end_stage(Outcome::Skipped)
    }

    /// Run the next stage. Any error from `work` marks it `Failed`.
    pub fn run<T, E>(
        &mut self,
        work: impl FnOnce(&dyn Pulse) -> Result<T, E>,
    ) -> Result<T, RunError<E>> {
        let result = match self.next_stage() {
            Ok(stage) => work(stage),
            Err(error) => return Err(RunError::Plan(error)),
        };
        self.complete(result, Outcome::Failed)
    }

    /// Run the next stage whose only errors are stop requests. Any error from
    /// `work` marks it `Cancelled`.
    pub fn run_stoppable<T, E>(
        &mut self,
        work: impl FnOnce(&dyn Pulse) -> Result<T, E>,
    ) -> Result<T, RunError<E>> {
        let result = match self.next_stage() {
            Ok(stage) => work(stage),
            Err(error) => return Err(RunError::Plan(error)),
        };
        self.complete(result, Outcome::Cancelled)
    }

    /// Run the next stage, marking it `Cancelled` for errors `is_stop`
    /// accepts and `Failed` for any other error. `is_stop` runs only on error.
    ///
    /// Use this when `work` calls another library whose error can mean either
    /// a stop or a failure.
    pub fn run_classified<T, E>(
        &mut self,
        is_stop: impl FnOnce(&E) -> bool,
        work: impl FnOnce(&dyn Pulse) -> Result<T, E>,
    ) -> Result<T, RunError<E>> {
        let result = match self.next_stage() {
            Ok(stage) => work(stage),
            Err(error) => return Err(RunError::Plan(error)),
        };
        let on_error = match &result {
            Err(error) if is_stop(error) => Outcome::Cancelled,
            _ => Outcome::Failed,
        };
        self.complete(result, on_error)
    }

    /// Run the next stage when `work` plans it, so `?` works on both its plan
    /// errors and its work errors. Work errors that `is_stop` accepts mark the
    /// stage `Cancelled`; other work errors and plan errors mark it `Failed`.
    /// The returned error is flat.
    ///
    /// `work` finishes the children it splits; `Stages` finishes the stage.
    pub fn run_nested<T, E>(
        &mut self,
        is_stop: impl FnOnce(&E) -> bool,
        work: impl FnOnce(&dyn Pulse) -> Result<T, RunError<E>>,
    ) -> Result<T, RunError<E>> {
        let result = match self.next_stage() {
            Ok(stage) => work(stage),
            Err(error) => return Err(RunError::Plan(error)),
        };
        let on_error = match &result {
            Err(RunError::Work(error)) if is_stop(error) => Outcome::Cancelled,
            _ => Outcome::Failed,
        };
        match self.complete(result, on_error) {
            Ok(value) => Ok(value),
            Err(error) => Err(error.flatten()),
        }
    }

    /// Finish the current stage as `Succeeded`, or as `on_error` if `result`
    /// is an error. A stage that cannot record an error outcome is recorded
    /// as abandoned, and the work error stays the one the caller sees.
    #[inline(never)]
    fn complete<T, E>(
        &mut self,
        result: Result<T, E>,
        on_error: Outcome,
    ) -> Result<T, RunError<E>> {
        match result {
            Ok(value) => match self.end_stage(Outcome::Succeeded) {
                Ok(()) => Ok(value),
                Err(error) => Err(RunError::Plan(error)),
            },
            Err(error) => {
                let _ = self.end_stage(on_error);
                Err(RunError::Work(error))
            }
        }
    }

    fn next_stage(&mut self) -> Result<&dyn Pulse, PlanError> {
        if self.running {
            // The last stage's work panicked and the caller caught the panic.
            let _ = self.end_stage(Outcome::Abandoned);
        }
        match self.pending.front() {
            _ if self.stopped => Err(PlanError::Finished),
            Some(stage) => {
                stage.start()?;
                self.running = true;
                Ok(stage.pulse())
            }
            None => Err(PlanError::NoMoreStages),
        }
    }

    /// Finish the current stage. After anything but success, finish the rest
    /// as `NotRun`.
    #[inline(never)]
    fn end_stage(&mut self, outcome: Outcome) -> Result<(), PlanError> {
        self.running = false;
        let Some(stage) = self.pending.pop_front() else {
            return Err(PlanError::NoMoreStages);
        };
        let finished = stage.finish(outcome);
        if finished.is_err() || !matches!(outcome, Outcome::Succeeded | Outcome::Skipped) {
            self.stopped = true;
            while let Some(stage) = self.pending.pop_front() {
                let _ = stage.finish(Outcome::NotRun);
            }
        }
        finished
    }

    /// Confirm that every declared stage ran. Remaining stages are recorded as
    /// abandoned. This does not finish the parent pulse.
    pub fn finish(self) -> Result<(), PlanError> {
        if self.stopped {
            Err(PlanError::Finished)
        } else if !self.pending.is_empty() {
            Err(PlanError::UnfinishedChildren)
        } else {
            Ok(())
        }
    }
}

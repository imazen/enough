//! Running a sequential plan.

use crate::{Child, Execution, Outcome, PhaseSpec, PlanError, Pulse};
use alloc::vec::IntoIter;
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
/// maps to. After an error, every later stage is finished as `Skipped`, and
/// the original error is returned unchanged.
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
#[derive(Debug)]
pub struct Stages<'a> {
    stages: IntoIter<Child<'a>>,
    stopped: bool,
}

impl<'a> Stages<'a> {
    /// Split `parent` into sequential stages, before any of their work starts.
    pub fn new(parent: &'a dyn Pulse, parts: &[PhaseSpec<'_>]) -> Result<Self, PlanError> {
        Ok(Self {
            stages: parent.split(Execution::Sequence, parts)?.into_iter(),
            stopped: false,
        })
    }

    /// Run the next stage. Any error from `work` marks it `Failed`.
    pub fn run<T, E>(
        &mut self,
        work: impl FnOnce(&dyn Pulse) -> Result<T, E>,
    ) -> Result<T, RunError<E>> {
        self.run_with(|_| Outcome::Failed, work)
    }

    /// Run the next stage whose only errors are stop requests. Any error from
    /// `work` marks it `Cancelled`.
    pub fn run_stoppable<T, E>(
        &mut self,
        work: impl FnOnce(&dyn Pulse) -> Result<T, E>,
    ) -> Result<T, RunError<E>> {
        self.run_with(|_| Outcome::Cancelled, work)
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
        self.run_with(
            |error| {
                if is_stop(error) {
                    Outcome::Cancelled
                } else {
                    Outcome::Failed
                }
            },
            work,
        )
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
        self.run_with(
            |error| match error {
                RunError::Work(error) if is_stop(error) => Outcome::Cancelled,
                _ => Outcome::Failed,
            },
            work,
        )
        .map_err(RunError::flatten)
    }

    fn run_with<T, E>(
        &mut self,
        outcome_for: impl FnOnce(&E) -> Outcome,
        work: impl FnOnce(&dyn Pulse) -> Result<T, E>,
    ) -> Result<T, RunError<E>> {
        if self.stopped {
            return Err(RunError::Plan(PlanError::Finished));
        }
        let stage = self
            .stages
            .next()
            .ok_or(RunError::Plan(PlanError::NoMoreStages))?;
        match work(&stage) {
            Ok(value) => match stage.finish(Outcome::Succeeded) {
                Ok(()) => Ok(value),
                Err(error) => {
                    self.skip_rest();
                    Err(RunError::Plan(error))
                }
            },
            Err(error) => {
                // A stage that cannot record its outcome is recorded as
                // abandoned; the work error stays the one the caller sees.
                let _ = stage.finish(outcome_for(&error));
                self.skip_rest();
                Err(RunError::Work(error))
            }
        }
    }

    fn skip_rest(&mut self) {
        self.stopped = true;
        for stage in self.stages.by_ref() {
            let _ = stage.finish(Outcome::Skipped);
        }
    }

    /// Confirm that every declared stage ran. Remaining stages are recorded as
    /// abandoned. This does not finish the parent pulse.
    pub fn finish(self) -> Result<(), PlanError> {
        if self.stopped {
            Err(PlanError::Finished)
        } else if self.stages.len() != 0 {
            Err(PlanError::UnfinishedChildren)
        } else {
            Ok(())
        }
    }
}

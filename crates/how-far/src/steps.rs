//! Sequential phase lifecycle for library-owned work.

use crate::{Execution, Outcome, PhaseSpec, PlanError, Pulse};
use alloc::{boxed::Box, vec::Vec};

/// An operation error or a progress-plan error.
#[derive(Debug)]
pub enum RunError<E> {
    /// The operation returned an error. Its active stage and later stages were
    /// marked according to the selected [`Steps`] method.
    Work(E),
    /// A phase could not be planned or finished.
    Plan(PlanError),
}
impl<E> From<PlanError> for RunError<E> {
    fn from(error: PlanError) -> Self {
        Self::Plan(error)
    }
}
impl<E: core::fmt::Display> core::fmt::Display for RunError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
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

/// Run the leaf stages of a sequential plan without manual outcome bookkeeping.
///
/// Declare all stages up front. Each `run` call receives the next child as
/// `&dyn Pulse`; it finishes that child on success. On error it finishes the
/// active child with the selected outcome, skips later children, and finishes
/// the parent. A panic leaves unfinished phases abandoned. Use
/// Share the stage's `&dyn Pulse` among workers that count one logical phase;
/// join them before the closure returns. Use [`Self::run_nested_stoppable`]
/// with [`Pulse::split`] when workers need separate child phases.
pub struct Steps<'a> {
    parent: &'a dyn Pulse,
    children: Vec<Box<dyn Pulse + 'a>>,
    next: usize,
    terminal: bool,
}
impl<'a> Steps<'a> {
    /// Partition `parent` into serial stages before any of their work starts.
    pub fn new(parent: &'a dyn Pulse, parts: &[PhaseSpec<'_>]) -> Result<Self, PlanError> {
        Ok(Self {
            parent,
            children: parent.split(Execution::Sequence, parts)?,
            next: 0,
            terminal: false,
        })
    }

    /// Run the next leaf stage. An operation error marks it `Failed`.
    pub fn run<T, E>(
        &mut self,
        work: impl FnOnce(&dyn Pulse) -> Result<T, E>,
    ) -> Result<T, RunError<E>> {
        self.run_with(|_| Outcome::Failed, work)
    }

    /// Run the next leaf stage when its only operation error is a stop request.
    /// An operation error marks it `Cancelled` and later stages `Skipped`.
    pub fn run_stoppable<T, E>(
        &mut self,
        work: impl FnOnce(&dyn Pulse) -> Result<T, E>,
    ) -> Result<T, RunError<E>> {
        self.run_with(|_| Outcome::Cancelled, work)
    }

    /// Run the next stage when its operation can either stop cooperatively or
    /// fail for another reason. Return `true` from `is_cancelled` only for a
    /// stop error; other errors mark the stage `Failed`. Later stages are
    /// skipped in either case. The classifier runs only when `work` fails.
    pub fn run_classified<T, E>(
        &mut self,
        is_cancelled: impl FnOnce(&E) -> bool,
        work: impl FnOnce(&dyn Pulse) -> Result<T, E>,
    ) -> Result<T, RunError<E>> {
        self.run_with(
            |error| {
                if is_cancelled(error) {
                    Outcome::Cancelled
                } else {
                    Outcome::Failed
                }
            },
            work,
        )
    }

    /// Run a stage that may itself plan and join child work. The stage should
    /// finish its children, then let `Steps` finish the stage itself. A nested
    /// operation error cancels this and later stages; a nested plan error
    /// marks this stage failed. The returned error stays flat.
    pub fn run_nested_stoppable<T, E>(
        &mut self,
        work: impl FnOnce(&dyn Pulse) -> Result<T, RunError<E>>,
    ) -> Result<T, RunError<E>> {
        self.run_with(
            |error| match error {
                RunError::Work(_) => Outcome::Cancelled,
                RunError::Plan(_) => Outcome::Failed,
            },
            work,
        )
        .map_err(RunError::flatten)
    }

    fn run_with<T, E>(
        &mut self,
        outcome_for_error: impl FnOnce(&E) -> Outcome,
        work: impl FnOnce(&dyn Pulse) -> Result<T, E>,
    ) -> Result<T, RunError<E>> {
        if self.terminal {
            return Err(RunError::Plan(PlanError::Finished));
        }
        let child = self
            .children
            .get(self.next)
            .ok_or(RunError::Plan(PlanError::NoMoreChildren))?;
        match work(child.as_ref()) {
            Ok(value) => {
                child.finish(Outcome::Succeeded)?;
                self.next += 1;
                Ok(value)
            }
            Err(error) => {
                let failure = outcome_for_error(&error);
                child.finish(failure)?;
                self.next += 1;
                for child in &self.children[self.next..] {
                    child.finish(Outcome::Skipped)?;
                }
                self.parent.finish(failure)?;
                self.terminal = true;
                Err(RunError::Work(error))
            }
        }
    }

    /// Finish the parent after every declared stage has succeeded.
    pub fn finish(self) -> Result<(), PlanError> {
        if self.terminal {
            return Err(PlanError::Finished);
        }
        if self.next != self.children.len() {
            return Err(PlanError::UnfinishedChildren);
        }
        self.parent.finish(Outcome::Succeeded)
    }
}

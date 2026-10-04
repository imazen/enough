//! Best-effort observation of sequential work; errors belong to the work.
use crate::{Child, Complete, Execution, Outcome, PhaseSpec, PlanError, Pulse, StopReason};
use alloc::collections::VecDeque;

/// Sequential work returning the library's ordinary `Result<T, E>`.
///
/// Implement `TryFrom<&YourError> for StopReason` to recognize cancellation.
/// `From<StopReason>` supports `?` at checks; no planning-error conversion is
/// required. For explicitly fallible planning use `TryStages` (the `checked` feature).
///
/// Pass the final result to `complete` (or `ResultExt::finish_phase`) to resolve
/// untouched stages. Drop alone records abandonment. Recovery belongs inside a
/// stage closure, using separate child owners for individual attempts.
#[must_use = "run stages and hand over the final result; Drop records abandonment"]
pub struct Stages<'a> {
    parent: &'a dyn Pulse,
    pending: VecDeque<Child<'a>>,
    state: State,
}
enum State {
    Ready,
    Running,
    Stopped,
}

impl<'a> Stages<'a> {
    /// Declare a sequence. Rejected observations retain cancellation through
    /// fallback children and cannot fail the work.
    #[track_caller]
    pub fn new(parent: &'a dyn Pulse, parts: &[PhaseSpec<'_>]) -> Self {
        Self {
            parent,
            pending: parent.plan(Execution::Sequence, parts).into(),
            state: State::Ready,
        }
    }

    /// Run the next stage, preserving the original result and error identity.
    /// After a stage error, later declared stages become NotRun. Calling again
    /// is diagnosed and runs against a cancellation-only view of the parent.
    #[track_caller]
    pub fn run<T, E>(&mut self, work: impl FnOnce(&dyn Pulse) -> Result<T, E>) -> Result<T, E>
    where
        for<'e> &'e E: TryInto<StopReason>,
    {
        let child = self.begin();
        let result = work(&child);
        self.complete_result(child, result)
    }

    /// Run with an explicit classifier for foreign error wrappers or a local
    /// classification policy, returning the work's original Result unchanged.
    #[track_caller]
    pub fn run_classified<T, E>(
        &mut self,
        is_stop: impl FnOnce(&E) -> bool,
        work: impl FnOnce(&dyn Pulse) -> Result<T, E>,
    ) -> Result<T, E> {
        let child = self.begin();
        let result = work(&child);
        self.end(child, Outcome::from_result(&result, is_stop));
        result
    }

    // Keep bookkeeping out of the per-closure monomorphization.
    fn complete_result<T, E>(&mut self, child: Child<'_>, result: Result<T, E>) -> Result<T, E>
    where
        for<'e> &'e E: TryInto<StopReason>,
    {
        let outcome = Outcome::from_result(&result, |error| error.try_into().is_ok());
        self.end(child, outcome);
        result
    }

    #[track_caller]
    fn begin(&mut self) -> Child<'a> {
        let child = self.next();
        if !matches!(self.state, State::Stopped) {
            self.state = State::Running;
        }
        if let Err(error) = child.start() {
            self.parent.record_issue(error);
        }
        child
    }

    fn end(&mut self, child: Child<'_>, outcome: Outcome) {
        child.complete_as(outcome);
        if matches!(outcome, Outcome::Failed | Outcome::Cancelled) {
            self.state = State::Stopped;
            self.resolve_remaining(Outcome::NotRun);
        } else if matches!(self.state, State::Running) {
            self.state = State::Ready;
        }
    }

    /// Explicitly mark the next phase unnecessary, making that decision visible now.
    #[track_caller]
    pub fn skip(&mut self) {
        self.next().complete_as(Outcome::Skipped);
    }

    #[track_caller]
    fn next(&mut self) -> Child<'a> {
        if matches!(self.state, State::Running) {
            // A prior closure panicked, and the caller caught it.
            self.state = State::Stopped;
            self.resolve_remaining(Outcome::NotRun);
        }
        if matches!(self.state, State::Stopped) {
            self.parent.record_issue(PlanError::Finished);
        } else if let Some(child) = self.pending.pop_front() {
            return child;
        } else {
            self.parent.record_issue(PlanError::NoMoreStages);
        }
        Child::untracked(self.parent)
    }

    fn resolve_remaining(&mut self, outcome: Outcome) {
        while let Some(child) = self.pending.pop_front() {
            child.complete_inferred(outcome);
        }
    }
}
impl Complete for Stages<'_> {
    fn complete_as(mut self, outcome: Outcome) {
        self.resolve_remaining(match outcome {
            Outcome::Succeeded | Outcome::Skipped => Outcome::Skipped,
            Outcome::Abandoned => Outcome::Abandoned,
            _ => Outcome::NotRun,
        });
    }
}

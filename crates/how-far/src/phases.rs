//! Independently selected phases for branches, attempts, and recoverable failures.
use crate::{Child, Complete, Execution, Outcome, PhaseSpec, PlanError, Pulse, StopReason};
use alloc::vec::Vec;

/// A fixed set of owned phases, selected by index rather than automatic sequence.
/// A failed attempt leaves other phases available. The library decides whether
/// to recover, retry, or propagate its error. Hand the final result to `complete`
/// to resolve untouched phases; Drop without that handoff records abandonment.
#[must_use = "run phases and hand over the final result; Drop records abandonment"]
pub struct Phases<'a> {
    parent: &'a dyn Pulse,
    children: Vec<Option<Child<'a>>>,
}
impl<'a> Phases<'a> {
    /// Declare a fixed plan. Rejected plans retain cancellation via untracked children.
    #[track_caller]
    pub fn new(parent: &'a dyn Pulse, execution: Execution, parts: &[PhaseSpec<'_>]) -> Self {
        Self {
            parent,
            children: parent
                .plan(execution, parts)
                .into_iter()
                .map(Some)
                .collect(),
        }
    }
    /// Run one phase at most once. A missing/repeated index is diagnosed and
    /// still runs the work through a cancellation-only view.
    #[track_caller]
    pub fn run<T, E>(
        &mut self,
        index: usize,
        work: impl FnOnce(&dyn Pulse) -> Result<T, E>,
    ) -> Result<T, E>
    where
        for<'e> &'e E: TryInto<StopReason>,
    {
        let child = self.begin(index);
        let result = work(&child);
        child.complete(result)
    }
    /// Run with an explicit classifier when the error type cannot implement the
    /// borrowed conversion. The original result is returned unchanged.
    #[track_caller]
    pub fn run_classified<T, E>(
        &mut self,
        index: usize,
        is_stop: impl FnOnce(&E) -> bool,
        work: impl FnOnce(&dyn Pulse) -> Result<T, E>,
    ) -> Result<T, E> {
        let child = self.begin(index);
        let result = work(&child);
        child.complete_classified(result, is_stop)
    }
    /// Explicitly discharge a phase which is unnecessary.
    #[track_caller]
    pub fn skip(&mut self, index: usize) {
        self.take(index).complete_as(Outcome::Skipped);
    }

    #[track_caller]
    fn begin(&mut self, index: usize) -> Child<'a> {
        let child = self.take(index);
        if let Err(error) = child.start() {
            self.parent.record_issue(error);
        }
        child
    }

    #[track_caller]
    fn take(&mut self, index: usize) -> Child<'a> {
        if let Some(Some(child)) = self.children.get_mut(index).map(Option::take) {
            return child;
        }
        self.parent.record_issue(PlanError::NoMoreStages);
        Child::untracked(self.parent)
    }
}
impl Complete for Phases<'_> {
    fn complete_as(self, outcome: Outcome) {
        let untouched = match outcome {
            Outcome::Succeeded | Outcome::Skipped => Outcome::Skipped,
            Outcome::Abandoned => Outcome::Abandoned,
            _ => Outcome::NotRun,
        };
        for child in self.children.into_iter().flatten() {
            child.complete_inferred(untouched);
        }
    }
}

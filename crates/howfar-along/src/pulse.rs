//! Connect the tiny `Pulse` interface to the optional phase tree.

use crate::{
    Execution, Observer, Outcome, Part, Phase, PlanError, Progress, Report, Stop, StopReason,
    sync::OwnerCell,
};
use alloc::{boxed::Box, vec::Vec};
use core::sync::atomic::{AtomicU8, Ordering};
use howfar::{PhaseSpec, Pulse};

/// An owned phase that exposes one `&dyn Pulse` to a library.
///
/// Create a [`Phase`], keep its observer, then move its owner into this adapter.
/// The same stop policy applies to every nested child. Reporting remains atomic;
/// planning and completion administer phase metadata only at cold boundaries.
pub struct PulseTree<'a> {
    owner: OwnerCell<Phase>,
    report: Progress,
    observer: Observer,
    stop: &'a dyn Stop,
    // 0=unplanned leaf, 1=reporting leaf, 2=partitioned, 3=terminal.
    activity: AtomicU8,
}
impl<'a> PulseTree<'a> {
    /// Attach a shared cancellation policy to a phase owner.
    pub fn new(phase: Phase, stop: &'a dyn Stop) -> Self {
        let report = phase.deferred_progress();
        let observer = phase.observer();
        Self {
            owner: OwnerCell::new(phase),
            report,
            observer,
            stop,
            activity: AtomicU8::new(0),
        }
    }
    /// Observe this phase from any thread, including after it finishes.
    pub fn observer(&self) -> Observer {
        self.observer.clone()
    }
    fn with_owner<R>(&self, f: impl FnOnce(&mut Phase) -> R) -> Result<R, PlanError> {
        let mut phase = self.owner.take().ok_or(PlanError::Busy)?;
        let result = f(&mut phase);
        self.owner.put(phase);
        Ok(result)
    }
}
impl Stop for PulseTree<'_> {
    #[inline]
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.stop.check()
    }
    #[inline]
    fn may_stop(&self) -> bool {
        self.stop.may_stop()
    }
}
impl Report for PulseTree<'_> {
    #[inline]
    #[track_caller]
    fn advance(&self, completed: u64) {
        if completed == 0 {
            return;
        }
        match self
            .activity
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) | Err(1) => self.report.advance(completed),
            Err(_) => {} // A branch or terminal phase is not a leaf counter.
        }
    }
    fn may_report(&self) -> bool {
        self.activity.load(Ordering::Relaxed) < 2 && self.report.may_report()
    }
}
impl Pulse for PulseTree<'_> {
    fn split(
        &self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Box<dyn Pulse + '_>>, PlanError> {
        // Convert before claiming this phase. The split itself runs outside
        // OwnerCell's platform lock, including allocation and metadata writes.
        let owned = parts
            .iter()
            .map(|part| {
                Part::new(part.name, part.weight, part.total)
                    .units(part.units)
                    .execution(part.execution)
            })
            .collect();
        self.activity
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| PlanError::AlreadyInUse)?;
        let result = self.with_owner(|phase| phase.split_vec(execution, owned));
        match result {
            Ok(Ok(children)) => Ok(children
                .into_iter()
                .map(|child| Box::new(Self::new(child, self.stop)) as Box<dyn Pulse>)
                .collect()),
            Ok(Err(error)) | Err(error) => {
                self.activity.store(0, Ordering::Release);
                Err(error)
            }
        }
    }
    fn finish(&self, outcome: Outcome) -> Result<(), PlanError> {
        let result = self.with_owner(|phase| phase.finish_with(outcome))?;
        if result.is_ok() {
            self.activity.store(3, Ordering::Release);
        }
        result
    }
}

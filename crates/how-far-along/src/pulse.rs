//! One lifecycle owner and shareable, non-owning pulse views.
use crate::{
    Child, ChildPulse, Execution, NodeId, Observer, Outcome, Phase, PhaseSpec, PlanError,
    ProgressWithStop, Pulse, PulseHandle, Report, Reporter, SharedPulse, Stop, StopReason, Total,
    sync::OwnerCell,
};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    fmt,
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
};

/// Owns the root of a tracked operation. Workers borrow it or use `share()`.
/// Finish after joining workers. Dropping this owner freezes abandonment even
/// when non-owning shared handles remain alive.
#[must_use = "finish the root after its work joins; dropping it unfinished records Abandoned"]
pub struct PulseTree {
    node: TreePulse,
}
impl PulseTree {
    /// Track a phase with a shared cancellation policy.
    pub fn new(phase: Phase, stop: impl Stop + 'static) -> Self {
        Self {
            node: TreePulse::new(
                phase,
                Arc::new(stop),
                #[cfg(feature = "callback")]
                None,
            ),
        }
    }
    /// Observe the root, including after its owner finishes.
    pub fn observer(&self) -> Observer {
        self.node.state.observer.clone()
    }
    /// The root's stable identity.
    pub fn id(&self) -> NodeId {
        self.node.state.observer.id()
    }
    /// Finish the root after workers and children join. On error it is abandoned.
    pub fn finish(self, outcome: Outcome) -> Result<(), PlanError> {
        self.node.state.finish(outcome)
    }
    #[cfg(feature = "callback")]
    pub(crate) fn with_callback(phase: Phase, callback: Arc<crate::callback::Dispatcher>) -> Self {
        Self {
            node: TreePulse::new(phase, Arc::new(crate::Unstoppable), Some(callback)),
        }
    }
}
impl fmt::Debug for PulseTree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PulseTree")
            .field("root", &self.id())
            .finish()
    }
}
const UNPLANNED: u8 = 0;
const COUNTING: u8 = 1;
const SPLIT: u8 = 2;
const FINISHED: u8 = 3;

// This state is shared. The TreePulse token, not the last Arc, owns completion.
struct State {
    owner: OwnerCell<Phase>,
    reporter: Reporter,
    observer: Observer,
    stop: Arc<dyn Stop>,
    activity: AtomicU8,
    closed: AtomicBool,
    #[cfg(feature = "callback")]
    callback: Option<Arc<crate::callback::Dispatcher>>,
}
struct TreePulse {
    state: Arc<State>,
}
#[derive(Clone)]
struct View {
    state: Arc<State>,
}
impl TreePulse {
    fn new(
        phase: Phase,
        stop: Arc<dyn Stop>,
        #[cfg(feature = "callback")] callback: Option<Arc<crate::callback::Dispatcher>>,
    ) -> Self {
        Self {
            state: Arc::new(State {
                reporter: phase.deferred_reporter(),
                observer: phase.observer(),
                owner: OwnerCell::new(phase),
                stop,
                activity: AtomicU8::new(UNPLANNED),
                closed: AtomicBool::new(false),
                #[cfg(feature = "callback")]
                callback,
            }),
        }
    }
}
impl State {
    fn complete(&self, outcome: Outcome) -> Result<(), PlanError> {
        self.with_owner(|phase| phase.record_outcome(outcome))??;
        self.activity.store(FINISHED, Ordering::Release);
        Ok(())
    }
    fn start(&self) -> Result<(), PlanError> {
        self.with_owner(Phase::start)?
    }
    fn set_total(&self, total: Total) -> Result<(), PlanError> {
        self.with_owner(|p| p.set_total(total))?
    }

    /// Administer the phase. Concurrent administrators, including shared
    /// views, run one at a time; a closed owner reports `Finished`.
    fn with_owner<R>(&self, f: impl FnOnce(&mut Phase) -> R) -> Result<R, PlanError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(PlanError::Finished);
        }
        self.owner.with(f).ok_or(PlanError::Finished)
    }
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.activity.store(FINISHED, Ordering::Release);
        // Waits only for an administrative call already under way; an
        // unfinished phase records Abandoned as it drops, outside the lock.
        drop(self.owner.take());
    }
    fn finish(&self, outcome: Outcome) -> Result<(), PlanError> {
        let result = self.with_owner(|phase| phase.finish_with(outcome))?;
        if result.is_ok() {
            self.activity.store(FINISHED, Ordering::Release);
        }
        result
    }
    fn split(
        &self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Child<'static>>, PlanError> {
        PhaseSpec::validate_split(parts)?;
        self.activity
            .compare_exchange(UNPLANNED, SPLIT, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|s| {
                if s == FINISHED {
                    PlanError::Finished
                } else {
                    PlanError::AlreadyInUse
                }
            })?;
        match self.with_owner(|phase| phase.split_vec(execution, parts)) {
            Ok(Ok(phases)) => {
                let mut children = Vec::with_capacity(phases.len());
                for phase in phases {
                    children.push(Child::new(TreePulse::new(
                        phase,
                        Arc::clone(&self.stop),
                        #[cfg(feature = "callback")]
                        self.callback.clone(),
                    )));
                }
                Ok(children)
            }
            Ok(Err(error)) | Err(error) => {
                // A concurrent owner drop must never resurrect planning.
                let _ = self.activity.compare_exchange(
                    SPLIT,
                    UNPLANNED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                Err(error)
            }
        }
    }
}
impl Stop for State {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.stop.check()?;
        #[cfg(feature = "callback")]
        if let Some(callback) = &self.callback {
            return callback.check(&self.observer);
        }
        Ok(())
    }
    fn may_stop(&self) -> bool {
        #[cfg(feature = "callback")]
        if self.callback.is_some() {
            return true;
        }
        self.stop.may_stop()
    }
}
impl Report for State {
    #[track_caller]
    fn advance(&self, n: u64) {
        if n == 0 {
            return;
        }
        // During a split the reporter decides: it ignores a phase that became
        // a branch, and counts work for one whose split failed.
        if self.activity.load(Ordering::Acquire) == COUNTING
            || matches!(
                self.activity.compare_exchange(
                    UNPLANNED,
                    COUNTING,
                    Ordering::AcqRel,
                    Ordering::Acquire
                ),
                Ok(_) | Err(COUNTING) | Err(SPLIT)
            )
        {
            self.reporter.advance(n);
        }
    }
    fn may_report(&self) -> bool {
        self.activity.load(Ordering::Relaxed) != FINISHED && self.reporter.may_report()
    }
}
impl Drop for TreePulse {
    fn drop(&mut self) {
        self.state.close();
    }
}
impl ChildPulse for TreePulse {
    fn complete_inferred(self: Box<Self>, outcome: Outcome) {
        let _ = self
            .state
            .with_owner(|phase| phase.complete_inferred(outcome));
    }
    fn complete_as(self: Box<Self>, outcome: Outcome) {
        let _ = self.state.complete(outcome);
    }
    fn finish(self: Box<Self>, outcome: Outcome) -> Result<(), PlanError> {
        self.state.finish(outcome)
    }
}

impl how_far::Complete for PulseTree {
    fn complete_as(self, outcome: Outcome) {
        let _ = self.node.state.complete(outcome);
    }
}
macro_rules! pulse_view {
    ($ty:ty, $field:ident) => {
        impl Stop for $ty {
            #[inline]
            #[track_caller]
            fn check(&self) -> Result<(), StopReason> {
                self.$field.check()
            }
            fn may_stop(&self) -> bool {
                self.$field.may_stop()
            }
        }
        impl Report for $ty {
            #[inline]
            #[track_caller]
            fn advance(&self, n: u64) {
                self.$field.advance(n)
            }
            fn may_report(&self) -> bool {
                self.$field.may_report()
            }
        }
        impl Pulse for $ty {
            fn split(
                &self,
                e: Execution,
                parts: &[PhaseSpec<'_>],
            ) -> Result<Vec<Child<'_>>, PlanError> {
                self.$field.split(e, parts)
            }
            fn handle(&self) -> PulseHandle {
                // Keep the legacy count-only shape; both views retain checkpoint
                // callbacks and lifecycle guards instead of bypassing them.
                ProgressWithStop::new(
                    Some(self.$field.clone() as Arc<dyn Stop>),
                    Some(self.$field.clone() as Arc<dyn Report>),
                )
            }
            fn start(&self) -> Result<(), PlanError> {
                self.$field.start()
            }
            fn set_total(&self, total: Total) -> Result<(), PlanError> {
                self.$field.set_total(total)
            }
            fn share(&self) -> Result<SharedPulse, PlanError> {
                Ok(SharedPulse::new(View {
                    state: self.$field.clone(),
                }))
            }
        }
    };
}
pulse_view!(TreePulse, state);
pulse_view!(View, state);
impl Stop for PulseTree {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.node.check()
    }
    fn may_stop(&self) -> bool {
        self.node.may_stop()
    }
}
impl Report for PulseTree {
    #[track_caller]
    fn advance(&self, n: u64) {
        self.node.advance(n)
    }
    fn may_report(&self) -> bool {
        self.node.may_report()
    }
}
impl Pulse for PulseTree {
    fn split(&self, e: Execution, p: &[PhaseSpec<'_>]) -> Result<Vec<Child<'_>>, PlanError> {
        self.node.split(e, p)
    }
    fn handle(&self) -> PulseHandle {
        self.node.handle()
    }
    fn start(&self) -> Result<(), PlanError> {
        self.node.start()
    }
    fn set_total(&self, t: Total) -> Result<(), PlanError> {
        self.node.set_total(t)
    }
    fn share(&self) -> Result<SharedPulse, PlanError> {
        self.node.share()
    }
}

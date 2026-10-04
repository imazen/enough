//! Checkpoint callbacks over the same accounting state as PulseTree.
use crate::{
    Child, Execution, NodeId, Observer, Outcome, Phase, PhaseSpec, PlanError, Pulse, PulseHandle,
    PulseTree, Report, SharedPulse, Snapshot, Stop, StopReason, Summary, Total,
};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    fmt,
    sync::atomic::{AtomicU8, Ordering},
};

/// A checkpoint's lazy observation. No tree walk occurs unless requested.
pub struct Checkpoint<'a> {
    root: &'a Observer,
    phase: &'a Observer,
}
impl Checkpoint<'_> {
    /// Stable identity of the checking phase.
    pub fn phase_id(&self) -> NodeId {
        self.phase.id()
    }
    /// Human-readable label; IDs disambiguate duplicate names.
    pub fn phase_name(&self) -> &str {
        self.phase.name()
    }
    /// Allocation-free whole-job observation. Walks the tree.
    pub fn summary(&self) -> Summary {
        self.root.summary()
    }
    /// Whole-job observation without waiting for a metadata lock.
    pub fn try_summary(&self) -> Option<Summary> {
        self.root.try_summary()
    }
    /// Copy the whole tree, allocating names and children.
    pub fn snapshot(&self) -> Snapshot {
        self.root.snapshot()
    }
    /// Copy the tree only if metadata is immediately available.
    pub fn try_snapshot(&self) -> Option<Snapshot> {
        self.root.try_snapshot()
    }
}
type Callback = dyn Fn(&Checkpoint<'_>) -> Result<(), StopReason> + Send + Sync;
pub(crate) struct Dispatcher {
    callback: Box<Callback>,
    root: Observer,
    stopped: AtomicU8,
}
impl Dispatcher {
    pub(crate) fn check(&self, phase: &Observer) -> Result<(), StopReason> {
        let code = self.stopped.load(Ordering::Acquire);
        if code != 0 {
            return Err(decode(code));
        }
        if self.root.is_finished() || phase.is_finished() {
            return Ok(());
        }
        let result = (self.callback)(&Checkpoint {
            root: &self.root,
            phase,
        });
        if let Err(reason) = result {
            let code = match reason {
                StopReason::TimedOut => 2,
                _ => 1,
            };
            let _ = self
                .stopped
                .compare_exchange(0, code, Ordering::AcqRel, Ordering::Acquire);
        }
        // A concurrent callback may have stopped while ours was running.
        match self.stopped.load(Ordering::Acquire) {
            0 => Ok(()),
            code => Err(decode(code)),
        }
    }
}
fn decode(code: u8) -> StopReason {
    if code == 2 {
        StopReason::TimedOut
    } else {
        StopReason::Cancelled
    }
}

/// A tracked root with a callback at each cancellation checkpoint.
/// Reports, phase finishes, and Drop never invoke the callback. `step` invokes
/// it once; call `Paced::finish` to flush and check a final partial batch.
/// Callbacks may overlap on different workers; no internal lock is held.
/// The first stop reason wins. Already-running callbacks may complete.
/// For thread-affine UI captures use `poll::LocalPoller` with an Observer.
#[must_use = "finish the root after its work joins; dropping it unfinished records Abandoned"]
pub struct FnPulse {
    tree: PulseTree,
}
impl FnPulse {
    /// Start an unknown-total root with a checkpoint callback.
    pub fn new(
        name: &str,
        callback: impl Fn(&Checkpoint<'_>) -> Result<(), StopReason> + Send + Sync + 'static,
    ) -> Self {
        Self::with_phase(Phase::new(name, Total::Unknown), callback)
    }
    /// Attach a callback to a configured root, including a count-only operation.
    pub fn with_phase(
        phase: Phase,
        callback: impl Fn(&Checkpoint<'_>) -> Result<(), StopReason> + Send + Sync + 'static,
    ) -> Self {
        let root = phase.observer();
        let dispatch = Arc::new(Dispatcher {
            root,
            callback: Box::new(callback),
            stopped: AtomicU8::new(0),
        });
        Self {
            tree: PulseTree::with_callback(phase, dispatch),
        }
    }
    /// Observe progress without dispatching the callback.
    pub fn observer(&self) -> Observer {
        self.tree.observer()
    }
    /// Publish the final result. Read the observer for the final display update.
    pub fn finish(self, outcome: Outcome) -> Result<(), PlanError> {
        self.tree.finish(outcome)
    }
}
impl fmt::Debug for FnPulse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FnPulse")
            .field("root", &self.tree.id())
            .finish()
    }
}

impl how_far::Complete for FnPulse {
    fn complete_as(self, outcome: Outcome) {
        self.tree.complete_as(outcome);
    }
}
impl Stop for FnPulse {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.tree.check()
    }
    fn may_stop(&self) -> bool {
        true
    }
}
impl Report for FnPulse {
    #[track_caller]
    fn advance(&self, n: u64) {
        self.tree.advance(n)
    }
    fn may_report(&self) -> bool {
        self.tree.may_report()
    }
}
impl Pulse for FnPulse {
    fn split(&self, e: Execution, p: &[PhaseSpec<'_>]) -> Result<Vec<Child<'_>>, PlanError> {
        self.tree.split(e, p)
    }
    fn handle(&self) -> PulseHandle {
        self.tree.handle()
    }
    fn start(&self) -> Result<(), PlanError> {
        self.tree.start()
    }
    fn set_total(&self, t: Total) -> Result<(), PlanError> {
        self.tree.set_total(t)
    }
    fn share(&self) -> Result<SharedPulse, PlanError> {
        self.tree.share()
    }
}

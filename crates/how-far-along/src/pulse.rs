//! A phase tree behind the `Pulse` interface.

use crate::{
    Child, ChildPulse, Execution, NodeId, Observer, Outcome, Phase, PhaseSpec, PlanError,
    ProgressWithStop, Pulse, PulseHandle, Report, Reporter, Stop, StopReason, sync::OwnerCell,
};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    fmt,
    sync::atomic::{AtomicU8, Ordering},
};

/// The root of a tracked operation, handed to a library as `&dyn Pulse`.
///
/// The tree records whatever the library declares: phases, weights, counts and
/// outcomes. Read it from any thread through [`observer`](Self::observer). When
/// the library returns, finish the root with the operation's outcome; dropping
/// it unfinished records [`Outcome::Abandoned`].
///
/// ```
/// use how_far_along::{Outcome, Phase, PulseTree, Total, Unstoppable};
///
/// let tree = PulseTree::new(Phase::new("resize", Total::Unknown), Unstoppable);
/// let observer = tree.observer();
/// // my_library::resize(&image, &tree)?;  // takes &dyn how_far::Pulse
/// tree.finish(Outcome::Succeeded)?;
/// assert!(observer.is_finished());
/// # Ok::<(), how_far_along::PlanError>(())
/// ```
///
/// The tree owns its stop policy, so it is `'static`: move it into a spawned
/// thread or share it through an `Arc` like any other value. Every phase in
/// the tree checks the same policy.
pub struct PulseTree {
    node: TreePulse,
}

impl PulseTree {
    /// Track `phase` and check `stop` at every cancellation checkpoint.
    pub fn new(phase: Phase, stop: impl Stop + 'static) -> Self {
        Self {
            node: TreePulse::new(phase, Arc::new(stop)),
        }
    }

    /// A read-only view of the whole tree, usable from any thread, including
    /// after the tree finishes.
    pub fn observer(&self) -> Observer {
        self.node.observer.clone()
    }

    /// The root phase's identity.
    pub fn id(&self) -> NodeId {
        self.node.observer.id()
    }

    /// Record the operation's outcome.
    ///
    /// Fails if a phase the library planned is still running, or if `outcome`
    /// is `Succeeded` or `Skipped` while one of them did not succeed. The root
    /// is then recorded as abandoned. [`Outcome::from_result`] maps a
    /// library's result to an outcome.
    pub fn finish(self, outcome: Outcome) -> Result<(), PlanError> {
        self.node.finish_with(outcome)
    }
}

impl fmt::Debug for PulseTree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PulseTree")
            .field("root", &self.id())
            .finish_non_exhaustive()
    }
}

impl Stop for PulseTree {
    #[inline]
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.node.check()
    }
    #[inline]
    fn may_stop(&self) -> bool {
        self.node.may_stop()
    }
}

impl Report for PulseTree {
    #[inline]
    #[track_caller]
    fn advance(&self, completed: u64) {
        self.node.advance(completed);
    }
    #[inline]
    fn may_report(&self) -> bool {
        self.node.may_report()
    }
}

impl Pulse for PulseTree {
    fn split(
        &self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Child<'_>>, PlanError> {
        self.node.split(execution, parts)
    }
    fn handle(&self) -> PulseHandle {
        self.node.handle()
    }
}

// What a phase may still do: count as a leaf, or split into a branch.
const UNPLANNED: u8 = 0;
const COUNTING: u8 = 1;
const SPLIT: u8 = 2;
const FINISHED: u8 = 3;

/// One phase of the tree behind the `Pulse` interface. Roots are wrapped in
/// `PulseTree`; children are handed out as `Child`.
struct TreePulse {
    owner: OwnerCell<Phase>,
    reporter: Reporter,
    observer: Observer,
    stop: Arc<dyn Stop>,
    activity: AtomicU8,
}

impl TreePulse {
    fn new(phase: Phase, stop: Arc<dyn Stop>) -> Self {
        Self {
            reporter: phase.deferred_reporter(),
            observer: phase.observer(),
            owner: OwnerCell::new(phase),
            stop,
            activity: AtomicU8::new(UNPLANNED),
        }
    }

    /// Run `f` on the phase owner. Planning and finishing happen outside the
    /// owner's lock; a concurrent second administrator gets `Busy`.
    fn with_owner<R>(&self, f: impl FnOnce(&mut Phase) -> R) -> Result<R, PlanError> {
        let mut phase = self.owner.take().ok_or(PlanError::Busy)?;
        let result = f(&mut phase);
        self.owner.put(phase);
        Ok(result)
    }

    fn finish_with(&self, outcome: Outcome) -> Result<(), PlanError> {
        let result = self.with_owner(|phase| phase.finish_with(outcome))?;
        if result.is_ok() {
            self.activity.store(FINISHED, Ordering::Release);
        }
        result
    }
}

impl Stop for TreePulse {
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

impl Report for TreePulse {
    #[inline]
    #[track_caller]
    fn advance(&self, completed: u64) {
        if completed == 0 {
            return;
        }
        // A counting leaf stays counting until it finishes, so a load settles
        // every report after the first; only the first needs the exchange
        // that claims the phase as a leaf. A branch or finished phase does
        // not count units.
        if self.activity.load(Ordering::Acquire) == COUNTING
            || matches!(
                self.activity.compare_exchange(
                    UNPLANNED,
                    COUNTING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ),
                Ok(_) | Err(COUNTING)
            )
        {
            self.reporter.advance(completed);
        }
    }
    fn may_report(&self) -> bool {
        // `false` must be permanent, so a split in progress (which may still
        // fail and revert) does not count; the reporter turns inert only once
        // the split is published.
        self.activity.load(Ordering::Relaxed) != FINISHED && self.reporter.may_report()
    }
}

impl Pulse for TreePulse {
    fn split(
        &self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Child<'_>>, PlanError> {
        self.activity
            .compare_exchange(UNPLANNED, SPLIT, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| PlanError::AlreadyInUse)?;
        match self.with_owner(|phase| phase.split_vec(execution, parts)) {
            Ok(Ok(phases)) => {
                let mut children = Vec::with_capacity(phases.len());
                for phase in phases {
                    children.push(Child::new(TreePulse::new(phase, Arc::clone(&self.stop))));
                }
                Ok(children)
            }
            Ok(Err(error)) | Err(error) => {
                self.activity.store(UNPLANNED, Ordering::Release);
                Err(error)
            }
        }
    }

    fn handle(&self) -> PulseHandle {
        ProgressWithStop::new(
            Some(Arc::clone(&self.stop)),
            Some(Arc::new(self.reporter.clone()) as Arc<dyn Report>),
        )
    }
}

impl ChildPulse for TreePulse {
    fn finish(self: Box<Self>, outcome: Outcome) -> Result<(), PlanError> {
        self.finish_with(outcome)
    }
}

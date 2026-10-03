//! A pulse made from one callback.

use crate::{
    Child, ChildPulse, Execution, Outcome, PhaseSpec, PlanError, ProgressWithStop, Pulse,
    PulseHandle, Report, Total,
};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    fmt,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
};
use enough::{Stop, StopReason};

/// What an [`FnPulse`] callback sees: the whole job's progress, and the phase
/// that just reported.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Progress<'a> {
    /// The whole job's completed share, from 0.0 to 1.0.
    ///
    /// Each split shares its phase's part among the children by weight. A
    /// phase with an `Exact` or `Estimated` total moves its part as it counts,
    /// up to its total; one with an `Unknown` total moves it when it finishes
    /// `Succeeded` or `Skipped`. Counts reported with no plan at all leave it
    /// at 0.0.
    pub fraction: f64,
    /// The reporting phase's name; empty for the pulse itself.
    pub phase: &'a str,
    /// What the reporting phase counts.
    pub units: &'a str,
    /// The units the reporting phase has completed.
    pub completed: u64,
    /// The reporting phase's total.
    pub total: Total,
    /// `Some` when the reporting phase just finished, with its outcome.
    pub outcome: Option<Outcome>,
}

/// A pulse made from one callback, which sees every report and can stop the
/// work.
///
/// The callback runs after each report and when a phase finishes, on the
/// thread that reported, possibly on several threads at once. Returning an
/// error stops the work: from then on every check, in every phase, returns
/// that reason, so the library stops at its next checkpoint, and the callback
/// is not called again. The first error wins.
///
/// ```
/// use how_far::{FnPulse, Pulse, StopReason};
/// use std::sync::Arc;
/// use std::sync::atomic::{AtomicBool, Ordering};
///
/// let interrupted = Arc::new(AtomicBool::new(false));
/// let flag = Arc::clone(&interrupted);
/// let pulse = FnPulse::new(move |progress| {
///     eprint!("\r{:3.0}%", progress.fraction * 100.0);
///     if flag.load(Ordering::Relaxed) {
///         Err(StopReason::Cancelled)
///     } else {
///         Ok(())
///     }
/// });
/// # fn encode(_: &[u8], _: &dyn Pulse) -> Result<(), StopReason> { Ok(()) }
/// encode(b"pixels", &pulse)?;
/// # Ok::<(), StopReason>(())
/// ```
///
/// It plans like any tracker: splits are validated, a phase splits once and
/// before it counts, and children finish before their parent. Libraries that
/// report rarely, or pace their checkpoints, call it rarely; a library that
/// steps on every row calls it on every row.
pub struct FnPulse {
    node: Arc<Node>,
}

/// One unit of progress is `1 / SCALE` of the job. 2^48 keeps the shares of
/// deeply nested splits exact enough, and converts to `f64` without rounding.
const SCALE: u64 = 1 << 48;

type Callback = dyn Fn(&Progress<'_>) -> Result<(), StopReason> + Send + Sync;

/// The callback, the stop it latched, and the job's completed share.
struct Shared {
    callback: Box<Callback>,
    /// 0 while running; otherwise the reason the callback stopped the work.
    stopped: AtomicU8,
    /// The job's completed share, in units of `1 / SCALE`.
    done: AtomicU64,
}

/// `StopReason` is non-exhaustive: a reason added later stops as `Cancelled`
/// until it gets its own code here.
fn encode(reason: StopReason) -> u8 {
    match reason {
        StopReason::TimedOut => 2,
        _ => 1,
    }
}

fn decode(code: u8) -> StopReason {
    if code == 2 {
        StopReason::TimedOut
    } else {
        StopReason::Cancelled
    }
}

const FRESH: u8 = 0;
const COUNTING: u8 = 1;
const SPLIT: u8 = 2;
const FINISHED: u8 = 3;

/// One phase. Kept in an `Arc` so its handle can outlive the borrow.
struct Node {
    shared: Arc<Shared>,
    parent: Option<Arc<Node>>,
    /// This phase's part of the job, in units of `1 / SCALE`.
    share: u64,
    /// The units at which a leaf has earned its whole share: its total, or 0
    /// for an `Unknown` one.
    cap: u64,
    /// `share * 2^64 / cap`, so a report earns `units * rate >> 64` with a
    /// multiplication instead of a division.
    rate: u128,
    total: Total,
    name: Box<str>,
    units: Box<str>,
    completed: AtomicU64,
    state: AtomicU8,
    /// Children not yet finished.
    running: AtomicUsize,
    /// Whether a child finished other than `Succeeded` or `Skipped`.
    unsuccessful: AtomicBool,
}

impl Node {
    fn new(
        shared: Arc<Shared>,
        parent: Option<Arc<Node>>,
        share: u64,
        part: &PhaseSpec<'_>,
    ) -> Self {
        let cap = match part.total {
            Total::Exact(total) | Total::Estimated(total) => total,
            Total::Unknown => 0,
        };
        let rate = match cap {
            0 => 0,
            cap => (u128::from(share) << 64) / u128::from(cap),
        };
        Self {
            shared,
            parent,
            share,
            cap,
            rate,
            total: part.total,
            name: Box::from(part.name),
            units: Box::from(part.units),
            completed: AtomicU64::new(0),
            state: AtomicU8::new(FRESH),
            running: AtomicUsize::new(0),
            unsuccessful: AtomicBool::new(false),
        }
    }

    fn credit(&self, amount: u64) {
        if amount != 0 {
            self.shared.done.fetch_add(amount, Ordering::Relaxed);
        }
    }

    /// The part of this phase's share that `completed` units have earned:
    /// never more than the share, since `cap * rate >> 64 <= share`.
    fn earned(&self, completed: u64) -> u64 {
        ((u128::from(completed.min(self.cap)) * self.rate) >> 64) as u64
    }

    /// Run the callback, unless the work is already stopped, and latch its
    /// error.
    fn notify(&self, completed: u64, outcome: Option<Outcome>) {
        let shared = &*self.shared;
        if shared.stopped.load(Ordering::Acquire) != 0 {
            return;
        }
        let done = shared.done.load(Ordering::Relaxed).min(SCALE);
        let progress = Progress {
            fraction: done as f64 / SCALE as f64,
            phase: &self.name,
            units: &self.units,
            completed,
            total: self.total,
            outcome,
        };
        if let Err(reason) = (shared.callback)(&progress) {
            let _ = shared.stopped.compare_exchange(
                0,
                encode(reason),
                Ordering::Release,
                Ordering::Relaxed,
            );
        }
    }

    fn split<'a>(
        node: &'a Arc<Node>,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Child<'a>>, PlanError> {
        let weight = PhaseSpec::validate_split(parts)?;
        match node
            .state
            .compare_exchange(FRESH, SPLIT, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => {}
            Err(FINISHED) => return Err(PlanError::Finished),
            Err(_) => return Err(PlanError::AlreadyInUse),
        }
        node.running.fetch_add(parts.len(), Ordering::Relaxed);
        let mut children = Vec::with_capacity(parts.len());
        let mut given = 0_u64;
        for (i, part) in parts.iter().enumerate() {
            // The last child takes what rounding left, so the shares add up
            // to the parent's exactly.
            let share = if i + 1 == parts.len() {
                node.share - given
            } else {
                (u128::from(node.share) * u128::from(part.weight) / u128::from(weight)) as u64
            };
            given += share;
            let child = Node::new(
                Arc::clone(&node.shared),
                Some(Arc::clone(node)),
                share,
                part,
            );
            children.push(Child::new(FnChild {
                node: Arc::new(child),
            }));
        }
        Ok(children)
    }

    fn handle(node: &Arc<Node>) -> PulseHandle {
        ProgressWithStop::new(
            Some(Arc::clone(node) as Arc<dyn Stop>),
            Some(Arc::clone(node) as Arc<dyn Report>),
        )
    }
}

impl Stop for Node {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        match self.shared.stopped.load(Ordering::Acquire) {
            0 => Ok(()),
            code => Err(decode(code)),
        }
    }
}

impl Report for Node {
    fn advance(&self, completed: u64) {
        let mut state = self.state.load(Ordering::Acquire);
        if state == FRESH {
            // Counting makes this phase a leaf: it can no longer split.
            state = match self.state.compare_exchange(
                FRESH,
                COUNTING,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => COUNTING,
                Err(now) => now,
            };
        }
        let before = self.completed.fetch_add(completed, Ordering::Relaxed);
        let after = before.saturating_add(completed);
        if state == COUNTING {
            self.credit(self.earned(after) - self.earned(before));
        }
        self.notify(after, None);
    }
}

/// Forward `Stop`, `Report` and `Pulse` to the node.
macro_rules! node_pulse {
    ($ty:ty) => {
        impl Stop for $ty {
            #[inline]
            fn check(&self) -> Result<(), StopReason> {
                self.node.check()
            }
        }
        impl Report for $ty {
            #[inline]
            fn advance(&self, completed: u64) {
                self.node.advance(completed);
            }
        }
        impl Pulse for $ty {
            fn split(
                &self,
                _: Execution,
                parts: &[PhaseSpec<'_>],
            ) -> Result<Vec<Child<'_>>, PlanError> {
                Node::split(&self.node, parts)
            }
            fn handle(&self) -> PulseHandle {
                Node::handle(&self.node)
            }
        }
    };
}

node_pulse!(FnPulse);
node_pulse!(FnChild);

impl FnPulse {
    /// A pulse that calls `callback` with each report. Return an error from
    /// it to stop the work.
    pub fn new(
        callback: impl Fn(&Progress<'_>) -> Result<(), StopReason> + Send + Sync + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            callback: Box::new(callback),
            stopped: AtomicU8::new(0),
            done: AtomicU64::new(0),
        });
        let root = PhaseSpec::new("", 1, Total::Unknown);
        Self {
            node: Arc::new(Node::new(shared, None, SCALE, &root)),
        }
    }

    /// The job's completed share, as the callback would see it now.
    pub fn fraction(&self) -> f64 {
        self.node.shared.done.load(Ordering::Relaxed).min(SCALE) as f64 / SCALE as f64
    }
}

impl fmt::Debug for FnPulse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FnPulse")
            .field("fraction", &self.fraction())
            .field("stopped", &self.node.check().err())
            .finish_non_exhaustive()
    }
}

/// A planned phase of an [`FnPulse`].
struct FnChild {
    node: Arc<Node>,
}

impl ChildPulse for FnChild {
    fn finish(self: Box<Self>, outcome: Outcome) -> Result<(), PlanError> {
        let node = &self.node;
        if node.running.load(Ordering::Acquire) != 0 {
            return Err(PlanError::UnfinishedChildren);
        }
        let succeeded = matches!(outcome, Outcome::Succeeded | Outcome::Skipped);
        if succeeded && node.unsuccessful.load(Ordering::Relaxed) {
            return Err(PlanError::UnsuccessfulChildren);
        }
        let state = node.state.swap(FINISHED, Ordering::AcqRel);
        if state != SPLIT && succeeded {
            // A finished leaf has earned its whole share, however it counted.
            // Its reports added what its count earned, so add the rest.
            let earned = node.earned(node.completed.load(Ordering::Relaxed));
            node.credit(node.share - earned);
        }
        if let Some(parent) = &node.parent {
            if !succeeded {
                parent.unsuccessful.store(true, Ordering::Relaxed);
            }
            parent.running.fetch_sub(1, Ordering::Release);
        }
        node.notify(node.completed.load(Ordering::Relaxed), Some(outcome));
        Ok(())
    }
}

impl Drop for FnChild {
    /// A child dropped unfinished is abandoned: its parent can no longer
    /// succeed.
    fn drop(&mut self) {
        if self.node.state.load(Ordering::Acquire) == FINISHED {
            return;
        }
        if let Some(parent) = &self.node.parent {
            parent.unsuccessful.store(true, Ordering::Relaxed);
            parent.running.fetch_sub(1, Ordering::Release);
        }
    }
}

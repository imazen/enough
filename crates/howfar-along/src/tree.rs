use crate::{
    Report,
    sync::{Counter, MetadataCell},
};
use alloc::{string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

pub use howfar::{Execution, Outcome, PlanError, Total};

/// Lifecycle state, independent of the counted fraction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Status {
    /// Work has not started.
    Pending,
    /// Started explicitly or by the first nonzero report.
    Running,
    /// A frozen terminal observation.
    Finished(Outcome),
}

/// One child's fixed relative budget in a partition.
#[derive(Clone, Debug)]
pub struct Part {
    name: String,
    weight: u64,
    total: Total,
    units: String,
    execution: Execution,
}
impl Part {
    /// Describe a child. Weights need not sum to 100; they are normalized once.
    pub fn new(name: impl Into<String>, weight: u64, total: Total) -> Self {
        Self {
            name: name.into(),
            weight,
            total,
            units: "items".into(),
            execution: Execution::Unspecified,
        }
    }
    /// Name the counted unit (for example `rows`, `bytes`, or `superblocks`).
    pub fn units(mut self, units: impl Into<String>) -> Self {
        self.units = units.into();
        self
    }
    /// Attach scheduling information without creating an executor.
    pub fn execution(mut self, execution: Execution) -> Self {
        self.execution = execution;
        self
    }
}

#[derive(Clone)]
struct Metadata {
    units: String,
    total: Total,
    revisions: Vec<Total>,
    execution: Execution,
    children: Vec<Arc<Node>>,
    frozen: Option<Snapshot>,
}
struct Node {
    id: usize,
    parent: Option<usize>,
    name: String,
    weight: u64,
    initial_total: Total,
    next_id: Arc<AtomicUsize>,
    issued: AtomicBool,
    branch: AtomicBool,
    // 0=pending, 1=running, 2=terminal. The outcome lives in the frozen snapshot.
    state: AtomicU8,
    completed: Counter,
    meta: MetadataCell<Metadata>,
}
impl Node {
    fn new(id: usize, parent: Option<usize>, part: Part, next_id: Arc<AtomicUsize>) -> Self {
        Self {
            id,
            parent,
            name: part.name,
            weight: part.weight,
            initial_total: part.total,
            next_id,
            issued: AtomicBool::new(false),
            branch: AtomicBool::new(false),
            state: AtomicU8::new(0),
            completed: Counter::new(),
            meta: MetadataCell::new(Metadata {
                units: part.units,
                total: part.total,
                revisions: Vec::new(),
                execution: part.execution,
                children: Vec::new(),
                frozen: None,
            }),
        }
    }
    fn snapshot(&self) -> Snapshot {
        self.snapshot_with(false)
            .expect("blocking metadata read succeeds")
    }
    fn snapshot_with(&self, nonblocking: bool) -> Option<Snapshot> {
        let meta = if nonblocking {
            self.meta.try_get()?
        } else {
            self.meta.get()
        };
        if let Some(frozen) = &meta.frozen {
            return Some(frozen.clone());
        }
        let mut result = Snapshot {
            id: self.id,
            parent: self.parent,
            name: self.name.clone(),
            weight: self.weight,
            units: meta.units.clone(),
            total: meta.total,
            initial_total: self.initial_total,
            total_revisions: meta.revisions.clone(),
            execution: meta.execution,
            status: if self.state.load(Ordering::Acquire) == 0 {
                Status::Pending
            } else {
                Status::Running
            },
            completed: self.completed.get(),
            overflowed: self.completed.overflowed(),
            children: Vec::new(),
        };
        let children = meta.children.clone();
        result.children = children
            .iter()
            .map(|child| child.snapshot_with(nonblocking))
            .collect::<Option<Vec<_>>>()?;
        if result.status == Status::Pending
            && result.children.iter().any(|c| c.status != Status::Pending)
        {
            result.status = Status::Running;
        }
        Some(result)
    }
}

/// The unique owner of a phase's plan and outcome. Dropping it abandons the phase.
///
/// Clone [`Progress`] for workers, not the owner. Join workers and flush their
/// batches before calling [`finish`](Self::finish). A new attempt uses a new
/// phase/tree; old reporting handles can never reset or corrupt the new attempt.
pub struct Phase {
    node: Arc<Node>,
}
impl Phase {
    /// Create a job with one leaf, initially pending and counting `items`.
    pub fn new(name: impl Into<String>, total: Total) -> Self {
        Self {
            node: Arc::new(Node::new(
                0,
                None,
                Part::new(name, 1, total),
                Arc::new(AtomicUsize::new(1)),
            )),
        }
    }
    /// Obtain a cheap clonable reporting handle. Planning must precede this call.
    /// A branch handle is inert: only leaves count work, avoiding double counting.
    pub fn progress(&self) -> Progress {
        self.node.issued.store(true, Ordering::Relaxed);
        self.deferred_progress()
    }
    /// Build a reporter without starting or claiming the phase for planning.
    /// The Pulse adapter publishes its first report only after split is ruled out.
    pub(crate) fn deferred_progress(&self) -> Progress {
        Progress {
            node: Arc::clone(&self.node),
        }
    }
    /// Observe this subtree from any thread.
    pub fn observer(&self) -> Observer {
        Observer {
            node: Arc::clone(&self.node),
        }
    }
    /// Job-local stable identity, also used to associate profiling spans.
    pub fn id(&self) -> usize {
        self.node.id
    }
    /// Start an opaque phase (reports also start leaves automatically).
    pub fn start(&mut self) -> Result<(), PlanError> {
        self.ensure_live()?;
        self.node.state.store(1, Ordering::Release);
        Ok(())
    }
    /// Set units before planning/reporting begins. Units never change mid-count.
    pub fn set_units(&mut self, units: impl Into<String>) -> Result<(), PlanError> {
        self.ensure_unused()?;
        let units = units.into();
        let mut meta = (*self.node.meta.get()).clone();
        meta.units = units;
        self.node.meta.publish(meta);
        Ok(())
    }
    /// Attach an execution model before use, including a shared-counter work pool.
    pub fn set_execution(&mut self, execution: Execution) -> Result<(), PlanError> {
        self.ensure_unused()?;
        validate_execution(execution)?;
        let mut meta = (*self.node.meta.get()).clone();
        meta.execution = execution;
        self.node.meta.publish(meta);
        Ok(())
    }
    /// Revise a leaf denominator. Raw fractions may regress; consumers own smoothing.
    /// Every explicit revision is retained for this job, including revisions to exact counts.
    pub fn set_total(&mut self, total: Total) -> Result<(), PlanError> {
        self.ensure_live()?;
        if self.node.branch.load(Ordering::Relaxed) {
            return Err(PlanError::AlreadyInUse);
        }
        let mut meta = (*self.node.meta.get()).clone();
        if meta.total != total {
            meta.total = total;
            meta.revisions.push(total);
            self.node.meta.publish(meta);
        }
        Ok(())
    }
    /// Replace an unused leaf with a fixed weighted group, preserving array destructuring.
    ///
    /// ```
    /// use howfar_along::{Execution, Part, Phase, Total};
    /// let mut job = Phase::new("encode", Total::Unknown);
    /// let [before, middle, after] = job.split(Execution::Sequence, [
    ///     Part::new("prepare", 35, Total::Exact(1)),
    ///     Part::new("parallel", 30, Total::Unknown),
    ///     Part::new("write", 35, Total::Exact(1)),
    /// ])?;
    /// # Ok::<(), howfar_along::PlanError>(())
    /// ```
    pub fn split<const N: usize>(
        &mut self,
        execution: Execution,
        parts: [Part; N],
    ) -> Result<[Phase; N], PlanError> {
        let children = self.split_vec(execution, Vec::from(parts))?;
        Ok(children
            .try_into()
            .unwrap_or_else(|_| unreachable!("same array length")))
    }
    /// Partition a runtime-sized, fully discovered collection. A live partition
    /// cannot grow; keep discovery in an unknown-total leaf until the plan is known.
    #[allow(deprecated)] // Atomic::try_update is newer than the Rust 1.88 MSRV.
    pub fn split_vec(
        &mut self,
        execution: Execution,
        parts: Vec<Part>,
    ) -> Result<Vec<Phase>, PlanError> {
        self.ensure_unused()?;
        validate_execution(execution)?;
        if parts.is_empty() || parts.iter().any(|p| p.weight == 0) {
            return Err(PlanError::EmptyOrZeroWeight);
        }
        let mut sum = 0_u64;
        for part in &parts {
            sum = sum.checked_add(part.weight).ok_or(PlanError::Overflow)?;
            validate_execution(part.execution)?;
        }
        let first = self
            .node
            .next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                n.checked_add(parts.len())
            })
            .map_err(|_| PlanError::Overflow)?;
        let children: Vec<_> = parts
            .into_iter()
            .enumerate()
            .map(|(i, part)| Phase {
                node: Arc::new(Node::new(
                    first + i,
                    Some(self.node.id),
                    part,
                    Arc::clone(&self.node.next_id),
                )),
            })
            .collect();
        let mut meta = (*self.node.meta.get()).clone();
        meta.execution = execution;
        meta.total = Total::Unknown;
        meta.children = children.iter().map(|p| Arc::clone(&p.node)).collect();
        self.node.meta.publish(meta);
        self.node.branch.store(true, Ordering::Release);
        Ok(children)
    }
    /// Publish success after all workers joined and all children succeeded/skipped.
    pub fn finish(&mut self) -> Result<(), PlanError> {
        self.finish_with(Outcome::Succeeded)
    }
    /// Publish a terminal outcome and freeze this subtree's observation.
    /// Explicit outcomes require every child to be terminal; drop records abandonment
    /// immediately instead. Cancellation here describes an outcome, not a stop request.
    pub fn finish_with(&mut self, outcome: Outcome) -> Result<(), PlanError> {
        self.ensure_live()?;
        let mut snapshot = self.node.snapshot();
        if snapshot
            .children
            .iter()
            .any(|c| !matches!(c.status, Status::Finished(_)))
        {
            return Err(PlanError::UnfinishedChildren);
        }
        if matches!(outcome, Outcome::Succeeded | Outcome::Skipped)
            && snapshot.children.iter().any(|c| {
                !matches!(
                    c.status,
                    Status::Finished(Outcome::Succeeded | Outcome::Skipped)
                )
            })
        {
            return Err(PlanError::UnsuccessfulChildren);
        }
        snapshot.status = Status::Finished(outcome);
        self.freeze(snapshot);
        Ok(())
    }
    fn freeze(&mut self, snapshot: Snapshot) {
        let mut meta = (*self.node.meta.get()).clone();
        meta.frozen = Some(snapshot);
        self.node.meta.publish(meta);
        self.node.state.store(2, Ordering::Release);
    }
    fn ensure_live(&self) -> Result<(), PlanError> {
        if self.node.state.load(Ordering::Acquire) == 2 {
            Err(PlanError::Finished)
        } else {
            Ok(())
        }
    }
    fn ensure_unused(&self) -> Result<(), PlanError> {
        self.ensure_live()?;
        if self.node.issued.load(Ordering::Relaxed)
            || self.node.state.load(Ordering::Relaxed) != 0
            || self.node.branch.load(Ordering::Relaxed)
        {
            Err(PlanError::AlreadyInUse)
        } else {
            Ok(())
        }
    }
}
impl Drop for Phase {
    fn drop(&mut self) {
        if self.node.state.load(Ordering::Acquire) != 2 {
            let mut snapshot = self.node.snapshot();
            snapshot.status = Status::Finished(Outcome::Abandoned);
            self.freeze(snapshot);
        }
    }
}
fn validate_execution(execution: Execution) -> Result<(), PlanError> {
    if matches!(execution, Execution::WorkPool { max_parallelism: 0 }) {
        Err(PlanError::ZeroParallelism)
    } else {
        Ok(())
    }
}

/// A clonable, thread-safe leaf reporter. No clocks, callbacks, or tree walks on advance.
/// Counters saturate on overflow and expose that fact. Reports after finish are ignored.
#[derive(Clone)]
pub struct Progress {
    node: Arc<Node>,
}
impl Report for Progress {
    #[inline]
    #[track_caller]
    #[allow(clippy::collapsible_match)] // Keep the first-report transition explicit.
    fn advance(&self, completed: u64) {
        if completed == 0 || self.node.branch.load(Ordering::Relaxed) {
            return;
        }
        self.node.issued.store(true, Ordering::Relaxed);
        match self.node.state.load(Ordering::Acquire) {
            2 => return,
            0 => {
                if self
                    .node
                    .state
                    .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed)
                    == Err(2)
                {
                    return;
                }
            }
            _ => {}
        }
        self.node.completed.add(completed);
    }
    fn may_report(&self) -> bool {
        !self.node.branch.load(Ordering::Relaxed)
    }
}

/// A read-only, clonable view. Holding it retains this job's records.
#[derive(Clone)]
pub struct Observer {
    node: Arc<Node>,
}
impl Observer {
    /// Materialize a snapshot now. Live counters across nodes are weakly consistent;
    /// metadata for each node is coherent. Terminal subtree snapshots are frozen.
    /// This allocates and walks the observed tree; sampling cadence belongs to you.
    pub fn snapshot(&self) -> Snapshot {
        self.node.snapshot()
    }
    /// Sample without waiting for a contended std mutex. Returns `None` if any
    /// node is busy; retry on a later UI/event-loop turn. Never spins or retries.
    /// In no_std builds, the platform critical-section provider still controls
    /// entry into its critical section. Allocation and tree walks run outside it.
    pub fn try_snapshot(&self) -> Option<Snapshot> {
        self.node.snapshot_with(true)
    }
    /// Whether a terminal snapshot has been published, without walking the tree.
    pub fn is_finished(&self) -> bool {
        self.node.state.load(Ordering::Acquire) == 2
    }
}

/// Owned observation. Count fractions describe weighted work, never elapsed runtime or ETA.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Snapshot {
    /// Stable identifier within this job (root = 0).
    pub id: usize,
    /// Parent identifier, absent for the root.
    pub parent: Option<usize>,
    /// Phase label.
    pub name: String,
    /// Fixed relative weight among siblings.
    pub weight: u64,
    /// Name of the counted unit.
    pub units: String,
    /// Latest denominator.
    pub total: Total,
    /// Original denominator, before any revisions.
    pub initial_total: Total,
    /// Explicit denominator revisions, in order.
    pub total_revisions: Vec<Total>,
    /// Declared scheduling relationship.
    pub execution: Execution,
    /// Explicit lifecycle; 100% counted does not finish the phase.
    pub status: Status,
    /// Actual reported units (saturated on overflow).
    pub completed: u64,
    /// The true count could not be represented. No count fraction is valid.
    pub overflowed: bool,
    /// Children in declared plan order.
    pub children: Vec<Snapshot>,
}
impl Snapshot {
    /// Write a versioned JSON progress tree, including execution relationships,
    /// fixed weights, units, total revisions, outcomes, and overflow diagnostics.
    /// Counts and weights use decimal strings to preserve u64 precision in JavaScript.
    pub fn write_json(&self, out: &mut impl core::fmt::Write) -> core::fmt::Result {
        out.write_str("{\"schema_version\":1,\"root\":")?;
        self.write_node_json(out)?;
        out.write_char('}')
    }
    fn write_node_json(&self, out: &mut impl core::fmt::Write) -> core::fmt::Result {
        use crate::json::quote;
        write!(out, "{{\"id\":{},\"parent\":", self.id)?;
        match self.parent {
            Some(id) => write!(out, "{id}")?,
            None => out.write_str("null")?,
        }
        out.write_str(",\"name\":")?;
        quote(out, &self.name)?;
        write!(out, ",\"weight\":\"{}\",\"units\":", self.weight)?;
        quote(out, &self.units)?;
        out.write_str(",\"total\":")?;
        write_total(out, self.total)?;
        out.write_str(",\"initial_total\":")?;
        write_total(out, self.initial_total)?;
        out.write_str(",\"total_revisions\":[")?;
        for (i, total) in self.total_revisions.iter().enumerate() {
            if i > 0 {
                out.write_char(',')?;
            }
            write_total(out, *total)?;
        }
        let (execution, capacity) = match self.execution {
            Execution::Unspecified => ("Unspecified", None),
            Execution::Sequence => ("Sequence", None),
            Execution::ForkJoin => ("ForkJoin", None),
            Execution::WorkPool { max_parallelism } => ("WorkPool", Some(max_parallelism)),
            _ => ("Other", None),
        };
        write!(out, "],\"execution\":\"{execution}\",\"max_parallelism\":")?;
        match capacity {
            Some(n) => write!(out, "{n}")?,
            None => out.write_str("null")?,
        }
        let (status, outcome) = match self.status {
            Status::Pending => ("Pending", None),
            Status::Running => ("Running", None),
            Status::Finished(outcome) => ("Finished", Some(outcome)),
        };
        write!(out, ",\"status\":\"{status}\",\"outcome\":")?;
        match outcome {
            Some(outcome) => write!(out, "\"{outcome:?}\"")?,
            None => out.write_str("null")?,
        }
        write!(
            out,
            ",\"completed\":\"{}\",\"overflowed\":{},\"overrun\":{},\"fraction\":",
            self.completed,
            self.overflowed,
            self.overrun()
        )?;
        match self.fraction() {
            Some(fraction) => write!(out, "{fraction}")?,
            None => out.write_str("null")?,
        }
        write!(
            out,
            ",\"unresolved_fraction\":{},\"children\":[",
            self.unresolved_fraction()
        )?;
        for (i, child) in self.children.iter().enumerate() {
            if i > 0 {
                out.write_char(',')?;
            }
            child.write_node_json(out)?;
        }
        out.write_str("]}")
    }
    /// Largest per-leaf count representable without a lock on this target.
    /// `u64::MAX` with 64-bit atomics (including wasm32); `usize::MAX` otherwise.
    /// Exceeding this limit sets `overflowed` rather than wrapping or blocking.
    pub const fn counter_max() -> u64 {
        #[cfg(target_has_atomic = "64")]
        {
            u64::MAX
        }
        #[cfg(not(target_has_atomic = "64"))]
        {
            usize::MAX as u64
        }
    }
    /// Work fraction in [0, 1], or None when any required denominator is unknown
    /// or invalid. Success/skipping discharges the obligation, including zero work.
    pub fn fraction(&self) -> Option<f64> {
        if matches!(
            self.status,
            Status::Finished(Outcome::Succeeded | Outcome::Skipped)
        ) {
            return Some(1.0);
        }
        if !self.children.is_empty() {
            let sum: f64 = self.children.iter().map(|c| c.weight as f64).sum();
            let mut result = 0.0;
            for child in &self.children {
                result += child.weight as f64 / sum * child.fraction()?;
            }
            return Some(result.clamp(0.0, 1.0));
        }
        if self.overflowed {
            return None;
        }
        match self.total {
            Total::Exact(n) | Total::Estimated(n) if n > 0 => {
                Some((self.completed as f64 / n as f64).min(1.0))
            }
            Total::Exact(0) | Total::Estimated(0) => Some(0.0),
            _ => None,
        }
    }
    /// Fraction of the fixed budget whose count denominator is unresolved.
    /// This lets consumers display "known work + unknown remainder" honestly.
    pub fn unresolved_fraction(&self) -> f64 {
        if self.fraction().is_some() {
            return 0.0;
        }
        if self.children.is_empty() {
            return 1.0;
        }
        let sum: f64 = self.children.iter().map(|c| c.weight as f64).sum();
        self.children
            .iter()
            .map(|c| c.weight as f64 / sum * c.unresolved_fraction())
            .sum()
    }
    /// Whether an exact count was exceeded. Success never hides this diagnostic.
    pub fn overrun(&self) -> bool {
        matches!(self.total, Total::Exact(n) if self.completed > n) || self.overflowed
    }
}

fn write_total(out: &mut impl core::fmt::Write, total: Total) -> core::fmt::Result {
    match total {
        Total::Unknown => out.write_str("{\"kind\":\"Unknown\"}"),
        Total::Exact(n) => write!(out, "{{\"kind\":\"Exact\",\"units\":\"{n}\"}}"),
        Total::Estimated(n) => write!(out, "{{\"kind\":\"Estimated\",\"units\":\"{n}\"}}"),
        _ => out.write_str("{\"kind\":\"Other\"}"),
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::Stop;
    use crate::poll::{Control, ControlHandle, LocalPoller};

    #[test]
    fn busy_child_skips_ui_snapshot_but_reporting_callbacks_and_cancel_still_work() {
        let mut root = Phase::new("root", Total::Unknown);
        let [child] = root
            .split(
                Execution::Sequence,
                [Part::new("child", 1, Total::Exact(3))],
            )
            .unwrap();
        let observer = root.observer();
        let control = ControlHandle::new();
        let mut poller = LocalPoller::new(observer.clone(), control.clone());
        poller.subscribe(|event| {
            assert!(event.try_snapshot().is_none());
            assert!(!event.snapshot_materialized());
            Control::Cancel
        });
        child.node.meta.with_lock_for_test(|| {
            child.progress().advance(1);
            assert!(observer.try_snapshot().is_none());
            assert!(poller.poll().cancelled);
            assert!(control.check().is_err());
        });
        assert_eq!(observer.try_snapshot().unwrap().children[0].completed, 1);
    }
}

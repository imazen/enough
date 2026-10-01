use crate::{
    Execution, Outcome, PhaseSpec, PlanError, Report, Total,
    sync::{Counter, MetadataCell},
};
use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{
    fmt,
    sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
};

/// A phase's identity within one job. The root is [`NodeId::ROOT`]; children
/// are numbered in planning order. Profiling spans use it to refer to phases.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(usize);

impl NodeId {
    /// The root phase of every job.
    pub const ROOT: Self = Self(0);

    /// The identifier as a number, as written in JSON exports.
    pub const fn get(self) -> usize {
        self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

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

#[derive(Clone)]
struct Metadata {
    units: String,
    total: Total,
    revisions: Vec<Total>,
    execution: Execution,
    children: Vec<Arc<Node>>,
    /// A snapshot taken at the outcome, kept only when the subtree could still
    /// change afterwards (see [`Phase::seal`]).
    frozen: Option<Snapshot>,
}

/// Outcomes as stored in [`Node::outcome`]; `UNKNOWN` is an outcome this
/// version cannot name, which is kept in a frozen snapshot instead.
const UNKNOWN: u8 = u8::MAX;
fn encode(outcome: Outcome) -> u8 {
    match outcome {
        Outcome::Succeeded => 0,
        Outcome::Skipped => 1,
        Outcome::Cancelled => 2,
        Outcome::Failed => 3,
        Outcome::Abandoned => 4,
        _ => UNKNOWN,
    }
}
fn decode(code: u8) -> Option<Outcome> {
    match code {
        0 => Some(Outcome::Succeeded),
        1 => Some(Outcome::Skipped),
        2 => Some(Outcome::Cancelled),
        3 => Some(Outcome::Failed),
        4 => Some(Outcome::Abandoned),
        _ => None,
    }
}
struct Node {
    id: NodeId,
    parent: Option<NodeId>,
    name: String,
    weight: u64,
    initial_total: Total,
    next_id: Arc<AtomicUsize>,
    issued: AtomicBool,
    branch: AtomicBool,
    // 0=pending, 1=running, 2=terminal. `outcome` and `final_count` are
    // written before the state becomes terminal.
    state: AtomicU8,
    outcome: AtomicU8,
    completed: Counter,
    /// The count at the outcome; a report racing with the finish is not shown.
    final_count: Counter,
    meta: MetadataCell<Metadata>,
}
impl Node {
    fn new(
        id: NodeId,
        parent: Option<NodeId>,
        spec: &PhaseSpec<'_>,
        next_id: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            id,
            parent,
            name: spec.name.to_string(),
            weight: spec.weight,
            initial_total: spec.total,
            next_id,
            issued: AtomicBool::new(false),
            branch: AtomicBool::new(false),
            state: AtomicU8::new(0),
            outcome: AtomicU8::new(UNKNOWN),
            completed: Counter::new(),
            final_count: Counter::new(),
            meta: MetadataCell::new(Metadata {
                units: spec.units.to_string(),
                total: spec.total,
                revisions: Vec::new(),
                execution: spec.execution,
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
        // The state first: a terminal state guarantees the metadata read next
        // includes any frozen snapshot.
        let state = self.state.load(Ordering::Acquire);
        let meta = if nonblocking {
            self.meta.try_get()?
        } else {
            self.meta.get()
        };
        if let Some(frozen) = &meta.frozen {
            return Some(frozen.clone());
        }
        let (status, counter) = match state {
            0 => (Status::Pending, &self.completed),
            1 => (Status::Running, &self.completed),
            // Without a frozen snapshot, a terminal outcome is always encoded.
            _ => (
                Status::Finished(
                    decode(self.outcome.load(Ordering::Relaxed)).unwrap_or(Outcome::Abandoned),
                ),
                &self.final_count,
            ),
        };
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
            status,
            completed: counter.get(),
            overflowed: counter.overflowed(),
            children: Vec::new(),
        };
        let children = meta.children.clone();
        result.children = Vec::with_capacity(children.len());
        for child in &children {
            let child = child.snapshot_with(nonblocking)?;
            if result.status == Status::Pending && child.status != Status::Pending {
                // A pending branch whose child has started is running.
                result.status = Status::Running;
            }
            result.children.push(child);
        }
        Some(result)
    }
    /// The outcome, once there is one.
    fn outcome(&self) -> Option<Outcome> {
        if self.state.load(Ordering::Acquire) != 2 {
            return None;
        }
        if let Some(outcome) = decode(self.outcome.load(Ordering::Relaxed)) {
            return Some(outcome);
        }
        match &self.meta.get().frozen {
            Some(Snapshot {
                status: Status::Finished(outcome),
                ..
            }) => Some(*outcome),
            _ => Some(Outcome::Abandoned),
        }
    }
}

/// The owner of one phase: its plan, its counter, and its outcome.
///
/// Give workers cloned [`Reporter`]s, not the owner. Join workers and flush
/// their batches before [`finish`](Self::finish). Dropping an unfinished owner
/// records [`Outcome::Abandoned`]. Each attempt uses a new phase, so a stale
/// reporter from an earlier attempt can never change a new one.
///
/// Phases are owned and `'static`: move children into spawned threads or
/// tasks, or wrap any phase in a [`PulseTree`](crate::PulseTree) to hand it to
/// a library as `&dyn Pulse`.
pub struct Phase {
    node: Arc<Node>,
}
impl Phase {
    /// Start a job: a pending root leaf that counts `items`.
    pub fn new(name: impl Into<String>, total: Total) -> Self {
        let name = name.into();
        Self {
            node: Arc::new(Node::new(
                NodeId::ROOT,
                None,
                &PhaseSpec::new(&name, 1, total),
                Arc::new(AtomicUsize::new(1)),
            )),
        }
    }
    /// A cheap, cloneable counter for this phase's workers.
    ///
    /// Taking one ends planning: the phase can no longer split. A branch's
    /// reporter ignores reports, so work is never counted twice.
    pub fn reporter(&self) -> Reporter {
        self.node.issued.store(true, Ordering::Relaxed);
        self.deferred_reporter()
    }
    /// A reporter that does not end planning until its first report.
    pub(crate) fn deferred_reporter(&self) -> Reporter {
        Reporter {
            node: Arc::clone(&self.node),
        }
    }
    /// Observe this subtree from any thread.
    pub fn observer(&self) -> Observer {
        Observer {
            node: Arc::clone(&self.node),
        }
    }
    /// This phase's identity within its job.
    pub fn id(&self) -> NodeId {
        self.node.id
    }
    /// Mark the phase running before its first report, for work that starts
    /// long before it can count anything. A report starts a leaf on its own.
    pub fn start(&mut self) -> Result<(), PlanError> {
        self.ensure_live()?;
        self.node.state.store(1, Ordering::Release);
        Ok(())
    }
    /// Name the counted unit, before planning or counting begins.
    pub fn set_units(&mut self, units: impl Into<String>) -> Result<(), PlanError> {
        self.ensure_unused()?;
        let units = units.into();
        let mut meta = (*self.node.meta.get()).clone();
        meta.units = units;
        self.node.meta.publish(meta);
        Ok(())
    }
    /// Declare how this phase's work is scheduled, before planning or counting.
    pub fn set_execution(&mut self, execution: Execution) -> Result<(), PlanError> {
        self.ensure_unused()?;
        let mut meta = (*self.node.meta.get()).clone();
        meta.execution = execution;
        self.node.meta.publish(meta);
        Ok(())
    }
    /// Revise a leaf's total while it runs.
    ///
    /// Every revision is kept in [`Snapshot::total_revisions`]. A fraction can
    /// go down after a revision; smoothing is the display's job.
    pub fn set_total(&mut self, total: Total) -> Result<(), PlanError> {
        self.ensure_live()?;
        if self.node.branch.load(Ordering::Relaxed) {
            return Err(PlanError::NotALeaf);
        }
        let mut meta = (*self.node.meta.get()).clone();
        if meta.total != total {
            meta.total = total;
            meta.revisions.push(total);
            self.node.meta.publish(meta);
        }
        Ok(())
    }
    /// Split an unused leaf into weighted children, returned as an array.
    ///
    /// ```
    /// use how_far_along::{Execution, Phase, PhaseSpec, Total};
    /// let mut job = Phase::new("encode", Total::Unknown);
    /// let [before, middle, after] = job.split(Execution::Sequence, [
    ///     PhaseSpec::new("prepare", 35, Total::Exact(1)),
    ///     PhaseSpec::new("parallel", 30, Total::Unknown),
    ///     PhaseSpec::new("write", 35, Total::Exact(1)),
    /// ])?;
    /// # Ok::<(), how_far_along::PlanError>(())
    /// ```
    pub fn split<const N: usize>(
        &mut self,
        execution: Execution,
        parts: [PhaseSpec<'_>; N],
    ) -> Result<[Phase; N], PlanError> {
        match self.split_vec(execution, &parts)?.try_into() {
            Ok(children) => Ok(children),
            Err(_) => unreachable!("one child per part"),
        }
    }
    /// Split an unused leaf into a runtime-sized list of children.
    ///
    /// A split is final: a running phase cannot gain siblings later. Keep
    /// discovery in an `Unknown`-total leaf until the full list is known.
    #[allow(deprecated)] // Atomic::try_update is newer than the Rust 1.88 MSRV.
    pub fn split_vec(
        &mut self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Phase>, PlanError> {
        self.ensure_unused()?;
        if parts.is_empty() {
            return Err(PlanError::EmptyOrZeroWeight);
        }
        // Checked in one pass, in the order `how-far`'s `NoPulse` checks, so
        // both report the same error for the same plan.
        let mut sum = 0_u64;
        for part in parts {
            if part.weight == 0 {
                return Err(PlanError::EmptyOrZeroWeight);
            }
            sum = sum.checked_add(part.weight).ok_or(PlanError::Overflow)?;
        }
        let first = self
            .node
            .next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                n.checked_add(parts.len())
            })
            .map_err(|_| PlanError::Overflow)?;
        let mut children = Vec::with_capacity(parts.len());
        let mut nodes = Vec::with_capacity(parts.len());
        for (i, part) in parts.iter().enumerate() {
            let node = Arc::new(Node::new(
                NodeId(first + i),
                Some(self.node.id),
                part,
                Arc::clone(&self.node.next_id),
            ));
            nodes.push(Arc::clone(&node));
            children.push(Phase { node });
        }
        let mut meta = (*self.node.meta.get()).clone();
        meta.execution = execution;
        meta.total = Total::Unknown;
        meta.children = nodes;
        self.node.meta.publish(meta);
        self.node.branch.store(true, Ordering::Release);
        Ok(children)
    }
    /// Record success, after every worker has joined and every child has
    /// succeeded or been skipped.
    pub fn finish(&mut self) -> Result<(), PlanError> {
        self.finish_with(Outcome::Succeeded)
    }
    /// Record a terminal outcome. The subtree's snapshot stops changing.
    ///
    /// Every child must have finished first, and `Succeeded` or `Skipped` also
    /// requires every child to have succeeded or been skipped. A failed call
    /// changes nothing, so the owner can fix the cause and finish again.
    /// `Cancelled` records how the work ended; it does not request a stop.
    pub fn finish_with(&mut self, outcome: Outcome) -> Result<(), PlanError> {
        self.ensure_live()?;
        let mut unsuccessful = false;
        for child in &self.node.meta.get().children {
            match child.outcome() {
                None => return Err(PlanError::UnfinishedChildren),
                Some(Outcome::Succeeded | Outcome::Skipped) => {}
                Some(_) => unsuccessful = true,
            }
        }
        if unsuccessful && matches!(outcome, Outcome::Succeeded | Outcome::Skipped) {
            return Err(PlanError::UnsuccessfulChildren);
        }
        self.seal(outcome);
        Ok(())
    }
    /// Make `outcome` terminal. When every child has finished, the subtree can
    /// no longer change, so atomics suffice and nothing is allocated. A phase
    /// dropped while a child runs keeps a snapshot taken now, so its view does
    /// not follow the orphaned child; so does an outcome this version cannot
    /// encode.
    fn seal(&mut self, outcome: Outcome) {
        let code = encode(outcome);
        let mut running_child = false;
        for child in &self.node.meta.get().children {
            if child.state.load(Ordering::Acquire) != 2 {
                running_child = true;
            }
        }
        if code == UNKNOWN || running_child {
            let mut snapshot = self.node.snapshot();
            snapshot.status = Status::Finished(outcome);
            let mut meta = (*self.node.meta.get()).clone();
            meta.frozen = Some(snapshot);
            self.node.meta.publish(meta);
        }
        self.node.final_count.copy_from(&self.node.completed);
        self.node.outcome.store(code, Ordering::Relaxed);
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
            self.seal(Outcome::Abandoned);
        }
    }
}
/// A cloneable, thread-safe counter for one phase, from [`Phase::reporter`].
///
/// `advance` is one atomic add: no clock, callback, lock, or tree walk.
/// Counts saturate and record the overflow. Reports after the phase finishes,
/// and reports to a phase that split, are ignored.
#[derive(Clone)]
pub struct Reporter {
    node: Arc<Node>,
}
impl fmt::Debug for Reporter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reporter")
            .field("phase", &self.node.id)
            .finish_non_exhaustive()
    }
}
impl Report for Reporter {
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

/// A read-only, cloneable view of a phase and its subtree, usable from any
/// thread. Holding one keeps the job's records alive.
#[derive(Clone)]
pub struct Observer {
    node: Arc<Node>,
}
impl fmt::Debug for Observer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Observer")
            .field("phase", &self.node.id)
            .finish_non_exhaustive()
    }
}
impl Observer {
    /// The observed phase's identity.
    pub fn id(&self) -> NodeId {
        self.node.id
    }
    /// Take a snapshot now.
    ///
    /// Each phase's metadata is internally consistent; counters in different
    /// phases are read one after another, not atomically together. A finished
    /// subtree's snapshot is frozen. This allocates and walks the subtree, so
    /// the caller chooses how often to sample.
    pub fn snapshot(&self) -> Snapshot {
        self.node.snapshot()
    }
    /// Take a snapshot without waiting for a lock held by another thread.
    ///
    /// Returns `None` if any phase's metadata is being replaced at that moment;
    /// try again on a later UI or event-loop turn. It never spins. Without
    /// `std`, entry into the platform's critical section is up to its provider;
    /// allocation and the tree walk happen outside it.
    pub fn try_snapshot(&self) -> Option<Snapshot> {
        self.node.snapshot_with(true)
    }
    /// Whether the phase has an outcome, without walking the tree.
    pub fn is_finished(&self) -> bool {
        self.node.state.load(Ordering::Acquire) == 2
    }
}

/// A point-in-time copy of a phase and its subtree.
///
/// Fractions measure weighted, counted work. They are not elapsed time and
/// not an ETA: a nearly finished fork-join can still wait on one slow branch.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Snapshot {
    /// This phase's identity within its job.
    pub id: NodeId,
    /// The parent's identity; `None` for the root.
    pub parent: Option<NodeId>,
    /// Phase label.
    pub name: String,
    /// Weight relative to its siblings, fixed when the parent split.
    pub weight: u64,
    /// Name of the counted unit.
    pub units: String,
    /// The current total.
    pub total: Total,
    /// The total declared when the phase was planned.
    pub initial_total: Total,
    /// Every later revision of the total, in order.
    pub total_revisions: Vec<Total>,
    /// How this phase's work is scheduled.
    pub execution: Execution,
    /// Lifecycle. Counting to 100% does not finish a phase; an outcome does.
    pub status: Status,
    /// Units reported so far, saturating at [`Snapshot::counter_max`].
    pub completed: u64,
    /// The true count exceeded the counter, so no fraction is valid.
    pub overflowed: bool,
    /// Children, in declared order.
    pub children: Vec<Snapshot>,
}
impl Snapshot {
    /// Write this tree as versioned JSON: plan, weights, units, totals and
    /// their revisions, outcomes, and overflow flags.
    ///
    /// Counts and weights are decimal strings, so 64-bit values survive
    /// JavaScript. Later versions may add keys; readers should ignore keys they
    /// do not know.
    pub fn write_json(&self, out: &mut impl fmt::Write) -> fmt::Result {
        out.write_str("{\"schema_version\":1,\"root\":")?;
        self.write_node_json(out)?;
        out.write_char('}')
    }
    pub(crate) fn write_node_json(&self, out: &mut impl fmt::Write) -> fmt::Result {
        use crate::json::{outcome_name, quote};
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
            Execution::WorkPool {
                max_parallelism, ..
            } => ("WorkPool", Some(max_parallelism)),
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
            Some(outcome) => write!(out, "\"{}\"", outcome_name(outcome))?,
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
            let sum = self.child_weight_sum();
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
        let sum = self.child_weight_sum();
        let mut unresolved = 0.0;
        for child in &self.children {
            unresolved += child.weight as f64 / sum * child.unresolved_fraction();
        }
        unresolved
    }
    fn child_weight_sum(&self) -> f64 {
        let mut sum = 0.0;
        for child in &self.children {
            sum += child.weight as f64;
        }
        sum
    }
    /// Whether an exact count was exceeded. Success never hides this diagnostic.
    pub fn overrun(&self) -> bool {
        matches!(self.total, Total::Exact(n) if self.completed > n) || self.overflowed
    }
}

fn write_total(out: &mut impl fmt::Write, total: Total) -> fmt::Result {
    match total {
        Total::Unknown => out.write_str("{\"kind\":\"Unknown\"}"),
        Total::Exact(n) => write!(out, "{{\"kind\":\"Exact\",\"count\":\"{n}\"}}"),
        Total::Estimated(n) => write!(out, "{{\"kind\":\"Estimated\",\"count\":\"{n}\"}}"),
        _ => out.write_str("{\"kind\":\"Other\"}"),
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::Stop;
    use crate::poll::LocalPoller;
    use almost_enough::Stopper;

    #[test]
    fn a_report_racing_with_the_finish_is_not_shown() {
        let mut leaf = Phase::new("leaf", Total::Exact(3));
        leaf.reporter().advance(2);
        leaf.finish().unwrap();
        // A report that passed the state check just before the finish lands
        // after it; the finished snapshot keeps the count at the outcome.
        leaf.node.completed.add(5);
        let snapshot = leaf.observer().snapshot();
        assert_eq!(snapshot.completed, 2);
        assert_eq!(snapshot.status, Status::Finished(Outcome::Succeeded));
    }

    #[test]
    fn a_busy_child_skips_ui_snapshots_while_reports_callbacks_and_cancellation_continue() {
        let mut root = Phase::new("root", Total::Unknown);
        let [child] = root
            .split(
                Execution::Sequence,
                [PhaseSpec::new("child", 1, Total::Exact(3))],
            )
            .unwrap();
        let observer = root.observer();
        let stop = Stopper::new();
        let cancel = stop.clone();
        let mut poller = LocalPoller::new(observer.clone());
        poller.subscribe(move |event| {
            assert!(event.try_snapshot().is_none());
            assert!(!event.snapshot_materialized());
            cancel.cancel();
        });
        child.node.meta.with_lock_for_test(|| {
            child.reporter().advance(1);
            assert!(observer.try_snapshot().is_none());
            poller.poll();
            assert!(stop.check().is_err());
        });
        assert_eq!(observer.try_snapshot().unwrap().children[0].completed, 1);
    }
}

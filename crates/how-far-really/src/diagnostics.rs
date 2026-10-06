//! Checkpoint advice for library tests, normally used as a dev-dependency.
//!
//! Wrap the [`PulseTree`] you pass to a library in a [`DiagnosticPulse`], run
//! the library, then call [`Trace::diagnose`] on the profiler's trace. The
//! findings point at source lines: long stretches without a cancellation
//! check, long stretches without a report, very hot call sites, slow or
//! irregular callbacks, and stage weights that differ from measured time.
//! Code that uses only a stop policy can be measured with
//! [`Span::instrument`](crate::profile::Span::instrument) and diagnosed the
//! same way.
//!
//! These are heuristics. Clock reads and bookkeeping slow the measured run,
//! and one run cannot establish good weights or production latency.
//!
//! # Stability
//!
//! [`Options`], [`Finding`] and [`Kind`] are `#[non_exhaustive]`. Build
//! `Options` from `Default`, and keep a wildcard arm when matching `Kind`.
//! `Kind` and whether a finding appears are the contract. The wording of
//! [`Finding::evidence`] and [`Finding::advice`], the sample code, and the
//! exact thresholds are heuristics that improve between releases: do not
//! parse them or assert on their text in downstream tests.

use crate::{
    profile::{
        Profiler, SourceSite, Span, SpanInner, SpanKind, SpanRecord, Trace, advanced, checked,
        sorted_indices,
    },
    sync::Mutex,
};
use alloc::{boxed::Box, format, string::String, sync::Arc, vec, vec::Vec};
use core::{
    fmt,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use how_far::{
    Child, ChildPulse, Execution, Outcome, PhaseSpec, PlanError, Pulse, Report, Stop, StopReason,
};
use how_far_along::{NodeId, Observer, PulseTree, Snapshot, Status};

/// Thresholds for [`Trace::diagnose`]. Start from `Default` and adjust fields.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Options {
    /// Longest acceptable stretch without a cancellation check. Default 10 ms.
    pub stop_gap_target: Duration,
    /// Longest acceptable stretch without a report. Default 50 ms.
    pub report_gap_target: Duration,
    /// Longest acceptable callback. Default 10 ms.
    pub callback_budget: Duration,
    /// Longest acceptable time between two runs of one callback. Default 10 ms.
    pub callback_interval_target: Duration,
    /// Check rate above which a busy call site gets a frequency note.
    pub check_calls_per_second: f64,
    /// Report rate above which a busy call site gets a batching note.
    pub report_calls_per_second: f64,
    /// Calls a site must make before it gets a frequency note.
    pub minimum_calls: u64,
    /// Sequential stages must run at least this long in total before their
    /// weights are compared with measured time.
    pub minimum_stage_wall: Duration,
    /// Smallest difference between a stage's declared and measured share that
    /// produces weight advice.
    pub weight_difference: f64,
    /// A stage that took less than this share of the sequence's time is too
    /// small to calibrate from one run (a flush that is trivial for this input
    /// may not be for the next). It keeps its declared weight, as long as the
    /// stages held this way together own at most half the bar.
    pub negligible_stage_share: f64,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            stop_gap_target: Duration::from_millis(10),
            report_gap_target: Duration::from_millis(50),
            callback_budget: Duration::from_millis(10),
            callback_interval_target: Duration::from_millis(10),
            check_calls_per_second: 100_000.0,
            report_calls_per_second: 10_000.0,
            minimum_calls: 100,
            minimum_stage_wall: Duration::from_millis(30),
            weight_difference: 0.15,
            negligible_stage_share: 0.02,
        }
    }
}

/// What a finding is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Kind {
    /// A phase owner vanished without a result handoff.
    AbandonedPhase,
    /// An untouched phase was resolved from its enclosing result.
    InferredCompletion,
    /// A successful parent retained a failed attempt; recovery is visible.
    RecoveredFailure,
    /// Completed units disagree with an exact total.
    CountMismatch,
    /// A rejected plan or lifecycle operation was reported by a best-effort helper.
    ProtocolMisuse,
    /// A report arrived after completion or targeted a branch instead of a leaf.
    InvalidReport,
    /// A check returned cancellation, but the observed phase ended differently.
    StopOutcomeMismatch,
    /// The host recorded a cancellation request and return, but no check observed it.
    UnobservedCancellation,
    /// A task went too long without checking for cancellation.
    StopGap,
    /// A task went too long without reporting.
    ReportGap,
    /// A call site checked very often.
    CheckFrequency,
    /// A call site reported very often.
    ReportFrequency,
    /// A callback ran longer than its budget.
    CallbackDuration,
    /// A callback ran less often than the target.
    CallbackInterval,
    /// Sequential stages took very different shares of time than their weights.
    StageWeights,
    /// Dropped spans, saturated counts, or clock trouble limit the advice.
    IncompleteEvidence,
}

/// A reporting problem detected without changing a library's return value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Problem {
    /// Rejected administration or invalid phase selection.
    Plan(PlanError),
    /// A retained shared view reported after its owner completed.
    ReportAfterCompletion,
    /// Reports targeted a phase that delegates its units to children.
    ReportToBranch,
}

/// Source-located evidence retained independently from phase outcomes.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Incident {
    /// Phase associated with this incident.
    pub node: NodeId,
    /// What was observed.
    pub problem: Problem,
    /// The library's call site.
    pub site: SourceSite,
}

/// One finding: what was measured, and what to consider doing about it.
/// `evidence`, `advice` and `sample_code` are for people and may be reworded
/// in any release; branch on [`Kind`], not on their text.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Finding {
    /// What the finding is about.
    pub kind: Kind,
    /// The measurement, with task names and source locations.
    pub evidence: String,
    /// A suggested change or further measurement.
    pub advice: String,
    /// Rust code to copy, for stage-weight findings.
    pub sample_code: Option<String>,
}
impl fmt::Display for Finding {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(out, "{:?}: {}", self.kind, self.evidence)?;
        writeln!(out, "  {}", self.advice)?;
        if let Some(code) = &self.sample_code {
            writeln!(out, "{code}")?;
        }
        Ok(())
    }
}

fn finding(kind: Kind, evidence: String, advice: &str) -> Finding {
    Finding {
        kind,
        evidence,
        advice: advice.into(),
        sample_code: None,
    }
}
fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}
fn location(site: Option<SourceSite>, boundary: &str) -> String {
    match site {
        Some(site) => format!("{site}"),
        None => boundary.into(),
    }
}

/// Measures a library's checkpoints without changing its signature.
///
/// Wrap the [`PulseTree`] you would pass to the library, pass `&measured`
/// instead, and finish the wrapper when the library returns:
///
/// ```
/// use how_far_along::{Outcome, Phase, PulseTree, Total, Unstoppable};
/// use how_far_really::diagnostics::{DiagnosticPulse, Options};
/// use how_far_really::profile::{Profiler, StdClock};
///
/// let profiler = Profiler::new(StdClock::new(), 256);
/// let tree = PulseTree::new(Phase::new("encode", Total::Unknown), Unstoppable);
/// let measured = DiagnosticPulse::new(tree, &profiler);
/// let observer = measured.observer();
/// // let result = my_library::encode(&input, &measured);
/// measured.finish(Outcome::Succeeded)?;
/// let trace = profiler.snapshot().with_progress(observer.snapshot());
/// for finding in trace.diagnose(&Options::default()) {
///     eprintln!("{finding}");
/// }
/// # Ok::<(), how_far_along::PlanError>(())
/// ```
///
/// Every phase the library plans gets its own span, tied to its node in the
/// tree. A phase's span starts at its first check or report; a stage of a
/// sequential split starts when the previous stage finished, and the first
/// stage when the plan was made. So time spent before a stage's first
/// checkpoint is measured, and a stage that never checks at all still gets a
/// span. Workers that share one phase share its span; give each worker its
/// own [`Span`] when one worker's long silence matters.
///
/// A phase that splits hands its time to its children: its own span ends at
/// the split, and a sequential stage then records a [`SpanKind::Wait`] span
/// while it coordinates them. A stage is thereby timed from entry to exit,
/// and its children's work never counts as its own missing checkpoints.
///
/// [`Pulse::share`] returns an instrumented owned view, so checks made inside
/// code that owns its stop policy, such as a codec context, count toward the
/// stage that handed it out. `may_stop` and `may_report` are always `true`, so
/// libraries that skip calls on no-op pulses still make the calls measured.
/// Creating one turns on [`Profiler::set_report_timing`].
#[must_use = "finish the measured root after its work joins; dropping it unfinished records Abandoned"]
pub struct DiagnosticPulse {
    inner: PulseTree,
    meter: MeterOwner,
}

impl fmt::Debug for DiagnosticPulse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiagnosticPulse")
            .field("tree", &self.inner)
            .finish_non_exhaustive()
    }
}

impl DiagnosticPulse {
    /// Measure everything a library does through `tree`.
    pub fn new(tree: PulseTree, profiler: &Profiler) -> Self {
        profiler.set_report_timing(true);
        let observer = tree.observer();
        let name = observer.name().into();
        Self {
            meter: Meter::owner(observer, name, profiler, None, None),
            inner: tree,
        }
    }

    /// A read-only view of the measured tree.
    pub fn observer(&self) -> Observer {
        self.meter.observer.clone()
    }

    /// Record the operation's outcome in the tree, and close the root's span.
    pub fn finish(self, outcome: Outcome) -> Result<(), PlanError> {
        let Self { inner, meter } = self;
        let result = inner.finish(outcome);
        meter.close(if result.is_ok() {
            outcome
        } else {
            Outcome::Abandoned
        });
        result
    }
}

/// The measurement state shared by the root wrapper and every child.
struct MeterOwner(Arc<Meter>);
impl core::ops::Deref for MeterOwner {
    type Target = Arc<Meter>;
    fn deref(&self) -> &Arc<Meter> {
        &self.0
    }
}
impl Drop for MeterOwner {
    fn drop(&mut self) {
        self.close(Outcome::Abandoned);
    }
}
struct Meter {
    /// This phase's own node, not the root's.
    observer: Observer,
    profiler: Profiler,
    name: String,
    span: Mutex<SpanSlot>,
    /// A sequential stage: when it was entered (the previous stage's end).
    entered: Option<Arc<Mutex<Duration>>>,
    /// A sequential stage: where the next stage reads its entry time.
    exited: Option<Arc<Mutex<Duration>>>,
    /// A phase that split: its time belongs to its children's spans.
    has_children: AtomicBool,
    closed: AtomicBool,
}

/// The span this phase records into now, and the last one it closed.
#[derive(Default)]
struct SpanSlot {
    open: Option<Span>,
    /// Kept after close: late calls through retained shared views land in
    /// a finished span, which ignores them, instead of opening one that never ends.
    last: Option<Arc<SpanInner>>,
}
impl SpanSlot {
    /// Finish the open span, keeping it for late calls.
    fn retire(&mut self, outcome: Outcome) {
        if let Some(span) = self.open.take() {
            self.last = Some(span.shared());
            span.finish(outcome);
        }
    }
}

impl Meter {
    fn owner(
        observer: Observer,
        name: String,
        profiler: &Profiler,
        entered: Option<Arc<Mutex<Duration>>>,
        exited: Option<Arc<Mutex<Duration>>>,
    ) -> MeterOwner {
        MeterOwner(Arc::new(Meter {
            observer,
            profiler: profiler.clone(),
            name,
            span: Mutex::new(SpanSlot::default()),
            entered,
            exited,
            has_children: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }))
    }
    /// `inner.check()`, recorded in this phase's span while it has one.
    #[track_caller]
    fn check(&self, inner: &dyn Stop) -> Result<(), StopReason> {
        match self.span() {
            Some(span) => checked(&span, inner),
            None => inner.check(),
        }
    }
    /// `inner.advance(n)`, recorded like [`Self::check`].
    #[track_caller]
    fn advance(&self, inner: &dyn Report, completed: u64) {
        self.reporting(completed);
        match self.span() {
            Some(span) => advanced(&span, inner, completed),
            None => inner.advance(completed),
        }
    }
    #[track_caller]
    fn issue(&self, problem: Problem) {
        self.profiler.incident(Incident {
            node: self.observer.id(),
            problem,
            site: SourceSite::from_location(core::panic::Location::caller()),
        });
    }
    #[track_caller]
    fn reporting(&self, n: u64) {
        if n == 0 {
            return;
        }
        if self.closed.load(Ordering::Acquire) || self.observer.is_finished() {
            self.issue(Problem::ReportAfterCompletion);
        } else if self.has_children.load(Ordering::Relaxed) {
            self.issue(Problem::ReportToBranch);
        }
    }
    fn start_span(&self, kind: SpanKind, entered: Option<Duration>) -> Span {
        let node = Some(self.observer.id());
        let name = self.name.clone();
        self.profiler.start_span(None, node, name, kind, entered)
    }
    /// A work span from the stage's entry, if it is a sequential stage.
    fn start_work(&self) -> Span {
        let entered = self.entered.as_ref().map(|cell| *cell.lock());
        self.start_span(SpanKind::Work, entered)
    }

    /// This phase's span, started at its first checkpoint. After the phase
    /// closed, the span it closed, if any.
    fn span(&self) -> Option<Arc<SpanInner>> {
        let mut slot = self.span.lock();
        if let Some(span) = &slot.open {
            return Some(span.shared());
        }
        if self.closed.load(Ordering::Acquire) {
            return slot.last.clone();
        }
        let span = self.start_work();
        let shared = span.shared();
        slot.open = Some(span);
        Some(shared)
    }

    /// The phase now delegates its time to children. Time spent before the
    /// split is a segment of its own work, timed from entry for a sequential
    /// stage. A sequential stage's remaining time, coordinating its children,
    /// becomes a `Wait` span, so the stage is timed from entry to exit without
    /// treating the children's work as its own silence.
    fn delegate(&self) {
        let mut slot = self.span.lock();
        if self.closed.load(Ordering::Acquire) || self.has_children.swap(true, Ordering::Relaxed) {
            return;
        }
        let Some(entered) = &self.entered else {
            slot.retire(Outcome::Succeeded);
            return;
        };
        let now = self.profiler.now();
        // Work before the split without a checkpoint still took time.
        if slot.open.is_none() && *entered.lock() < now {
            slot.open = Some(self.start_work());
        }
        slot.retire(Outcome::Succeeded);
        slot.open = Some(self.start_span(SpanKind::Wait, Some(now)));
    }

    fn split<'s>(
        &self,
        inner: &'s dyn Pulse,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Child<'s>>, PlanError> {
        let children = inner.split(execution, parts)?;
        self.delegate();
        let nodes = self.observer.children();
        // cells[i] is stage i's entry and stage i-1's exit.
        let mut cells = Vec::new();
        if execution == Execution::Sequence {
            let planned = self.profiler.now();
            for _ in 0..=children.len() {
                cells.push(Arc::new(Mutex::new(planned)));
            }
        }
        let mut measured = Vec::with_capacity(children.len());
        for (index, child) in children.into_iter().enumerate() {
            // A sink with no node per part keeps measuring this one.
            let observer = nodes.get(index).unwrap_or(&self.observer).clone();
            let meter = Meter::owner(
                observer,
                parts[index].name.into(),
                &self.profiler,
                cells.get(index).cloned(),
                cells.get(index + 1).cloned(),
            );
            measured.push(Child::new(DiagnosticChild {
                inner: child,
                meter,
            }));
        }
        Ok(measured)
    }

    fn close(&self, outcome: Outcome) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut slot = self.span.lock();
        if slot.open.is_none()
            && outcome == Outcome::Succeeded
            && self.entered.is_some()
            && !self.has_children.load(Ordering::Relaxed)
        {
            // A leaf stage with no checkpoint still took time.
            slot.open = Some(self.start_work());
        }
        slot.retire(outcome);
        drop(slot);
        if let Some(exited) = &self.exited {
            // Read after the span closed, so the next stage never starts first.
            *exited.lock() = self.profiler.now();
        }
    }
}

fn find_node(snapshot: &Snapshot, id: NodeId) -> Option<&Snapshot> {
    if snapshot.id == id {
        return Some(snapshot);
    }
    for child in &snapshot.children {
        if let Some(found) = find_node(child, id) {
            return Some(found);
        }
    }
    None
}

/// A measured child phase.
struct DiagnosticChild<'a> {
    inner: Child<'a>,
    meter: MeterOwner,
}

impl ChildPulse for DiagnosticChild<'_> {
    fn complete_as(self: Box<Self>, outcome: Outcome) {
        let Self { inner, meter } = *self;
        how_far::Complete::complete_as(inner, outcome);
        meter.close(outcome);
    }
    fn complete_inferred(self: Box<Self>, outcome: Outcome) {
        let Self { inner, meter } = *self;
        inner.complete_inferred(outcome);
        meter.close(outcome);
    }

    fn finish(self: Box<Self>, outcome: Outcome) -> Result<(), PlanError> {
        let Self { inner, meter } = *self;
        let result = inner.finish(outcome);
        meter.close(if result.is_ok() {
            outcome
        } else {
            Outcome::Abandoned
        });
        result
    }
}

// An owned, non-owning view of one phase. Each call records into whatever span
// the phase has now, so a view retained past a split or the phase's close
// records consistently, and dropping it never ends a span.
struct DiagnosticView {
    inner: how_far::SharedPulse,
    meter: Arc<Meter>,
}
/// `Stop`, `Report` and `Pulse` for wrappers whose `inner` pulse is measured
/// by `meter`. `may_stop` and `may_report` are `true` so calls are always made.
macro_rules! measured {
    ($($wrapper:ty),*) => {$(
        impl Stop for $wrapper {
            #[track_caller]
            fn check(&self) -> Result<(), StopReason> {
                self.meter.check(&self.inner)
            }
            fn may_stop(&self) -> bool {
                true
            }
        }
        impl Report for $wrapper {
            #[track_caller]
            fn advance(&self, completed: u64) {
                self.meter.advance(&self.inner, completed);
            }
            fn may_report(&self) -> bool {
                true
            }
        }
        impl Pulse for $wrapper {
            #[track_caller]
            fn record_issue(&self, error: PlanError) {
                self.meter.issue(Problem::Plan(error));
            }
            fn split(
                &self,
                execution: Execution,
                parts: &[PhaseSpec<'_>],
            ) -> Result<Vec<Child<'_>>, PlanError> {
                self.meter.split(&self.inner, execution, parts)
            }
            fn start(&self) -> Result<(), PlanError> {
                self.inner.start()
            }
            fn set_total(&self, total: how_far::Total) -> Result<(), PlanError> {
                self.inner.set_total(total)
            }
            fn share(&self) -> Result<how_far::SharedPulse, PlanError> {
                Ok(how_far::SharedPulse::new(DiagnosticView {
                    inner: self.inner.share()?,
                    meter: Arc::clone(&self.meter),
                }))
            }
        }
    )*};
}
measured!(DiagnosticPulse, DiagnosticChild<'_>, DiagnosticView);

impl Trace {
    /// Analyze the retained spans and the attached progress tree, if any.
    ///
    /// Rates are per span's wall time, not exact intervals at one line.
    /// Report gaps need [`Profiler::set_report_timing`] on while recording.
    /// Callback findings need callback spans, which
    /// [`Profiler::measure_callback`] records around each callback.
    pub fn diagnose(&self, options: &Options) -> Vec<Finding> {
        let mut findings = Vec::new();
        for incident in &self.incidents {
            findings.push(finding(match incident.problem { Problem::Plan(_) => Kind::ProtocolMisuse, _ => Kind::InvalidReport }, format!("phase {} at {}: {:?}", incident.node, incident.site, incident.problem), "Plan before reporting, select each phase once, and join workers before completing their owner. The work result was preserved."));
        }
        if self.dropped_incidents != 0 {
            findings.push(finding(
                Kind::IncompleteEvidence,
                format!("{} reporting incidents omitted", self.dropped_incidents),
                "Increase the incident budget or fix repeated protocol misuse.",
            ));
        }
        if let Some(root) = &self.progress {
            lifecycle_findings(root, false, &mut findings);
        }
        // A request after the operation returned had nothing left to stop.
        if let (Some(cancelled), Some(returned)) = (self.cancelled_at, self.returned_at)
            && cancelled <= returned
            && self.observed_at.is_none()
        {
            findings.push(finding(Kind::UnobservedCancellation, "host recorded cancellation and operation return, but no instrumented checkpoint observed the stop".into(), "Check at entry and finish the final paced batch. A request racing completion or uninstrumented checks can also explain this; timing alone does not prove a bug."));
        }
        if self.dropped_spans != 0 || self.active_spans != 0 {
            findings.push(finding(
                Kind::IncompleteEvidence,
                format!(
                    "{} spans dropped; {} still active",
                    self.dropped_spans, self.active_spans
                ),
                "Increase profiler capacity and finish/join every task before diagnosing.",
            ));
        }
        for span in &self.spans {
            if span.stats.stopped_at.is_some() && span.outcome != Outcome::Cancelled {
                findings.push(finding(Kind::StopOutcomeMismatch, format!("task {:?} observed a stop but ended {:?}", span.task, span.outcome), "Propagate StopReason and implement the borrowed error conversion. If recovery is intentional, record each attempt separately."));
            }
            if span.stats.overflowed
                || span.stats.clock_regressions != 0
                || span.stats.unattributed_calls != 0
            {
                findings.push(finding(Kind::IncompleteEvidence, format!(
                        "task {:?}: overflow={}, clock regressions={}, unattributed calls={}",
                        span.task, span.stats.overflowed, span.stats.clock_regressions,
                        span.stats.unattributed_calls
                    ), "Use a monotonic shared clock and sufficient span/site capacity before tuning this task."));
                continue;
            }
            if span.kind != SpanKind::Work || span.outcome != Outcome::Succeeded {
                continue;
            }
            if span.stats.max_check_gap > options.stop_gap_target
                && span.elapsed() >= options.stop_gap_target
            {
                // The last site whose check ended the longest gap (no site's
                // gap exceeds the span's), or else the task's exit.
                let mut ending = String::from("task exit");
                for s in &span.stats.sites {
                    if s.max_gap_before_check == span.stats.max_check_gap {
                        ending = format!("{}:{}:{}", s.file, s.line, s.column);
                    }
                }
                let (covered, note) = cover(
                    self,
                    span,
                    span.stats.max_check_gap_start,
                    span.stats.max_check_gap,
                    options,
                );
                findings.push(finding(Kind::StopGap, format!(
                        "task {:?}: {:.2} ms without a check before {ending}{note}",
                        span.task,
                        ms(span.stats.max_check_gap),
                    ), if covered {
                        "This task's own checkpoints are sparse here, but the other task above checked at the target cadence throughout the interval. If it is the work inside this stage (for example an encoder holding its own Stop), cancellation is already covered and this is a measurement seam, not a latency problem. Otherwise add a cheap check() in the long-running section."
                    } else {
                        "Add a cheap check() in the long-running section; keep the stop-only cadence independent of reporting."
                    }));
            }
            if counts_units(self, span)
                && let Some(gap) = &span.stats.max_report_gap
                && gap.duration > options.report_gap_target
            {
                let own = gap.checks > 0 && gap.max_check_gap <= options.stop_gap_target;
                let (covered, note) = if own {
                    (true, String::new())
                } else {
                    cover(self, span, gap.start, gap.duration, options)
                };
                findings.push(finding(Kind::ReportGap, format!(
                        "task {:?}: {:.2} ms between {} and {}; stop checks inside it: {}, longest stop gap {:.2} ms{note}",
                        span.task, ms(gap.duration),
                        location(gap.from, "task entry"), location(gap.to, "task exit"),
                        gap.checks, ms(gap.max_check_gap)
                    ), if covered {
                        "Cancellation is checked at the target cadence inside this interval, so this is a progress-granularity seam rather than missing cancellation checks; more stop checks would not help. Finer progress needs a smaller reportable unit: sub-phases, an Estimated total, or a report from inside the long operation. Visible UI smoothness also depends on the consumer's polling cadence."
                    } else {
                        "No stop check at the target cadence was recorded inside this interval either. Consider reporting completed units and checking cancellation here; step(n) does both."
                    }));
            }
            let seconds = span.elapsed().as_secs_f64();
            if seconds > 0.0 {
                for &(kind, threshold, advice) in &[
                    (
                        Kind::CheckFrequency,
                        options.check_calls_per_second,
                        "Measure the stop policy's actual cost before thinning checks; keep the required cancellation latency.",
                    ),
                    (
                        Kind::ReportFrequency,
                        options.report_calls_per_second,
                        "Consider batching reports per worker while keeping cancellation checks at their current cadence.",
                    ),
                ] {
                    let sites = &span.stats.sites;
                    let calls = |index: usize| match kind {
                        Kind::CheckFrequency => sites[index].checks,
                        _ => sites[index].reports,
                    };
                    // Busiest first; ties in recorded order.
                    let order = sorted_indices(sites.len(), &|a, b| calls(b).cmp(&calls(a)));
                    for &index in &order[..order.len().min(3)] {
                        let site = &sites[index];
                        let calls = calls(index);
                        if calls < options.minimum_calls || (calls as f64 / seconds) <= threshold {
                            continue;
                        }
                        findings.push(finding(
                            kind,
                            format!(
                                "task {:?}: {} calls at {}:{}:{} in {:.2} ms ({:.0}/s)",
                                span.task,
                                calls,
                                site.file,
                                site.line,
                                site.column,
                                ms(span.elapsed()),
                                calls as f64 / seconds,
                            ),
                            advice,
                        ));
                    }
                }
            }
        }
        callback_findings(self, options, &mut findings);
        if self.dropped_spans == 0
            && self.active_spans == 0
            && let Some(root) = &self.progress
        {
            stage_findings(self, root, options, &mut findings);
        }
        findings
    }
}

/// Another work span that ran across nearly all of `[start, start + len]` and
/// recorded checks, such as an encoder's own `Stop` instrumented from the same
/// profiler; among several, the first with the shortest longest gap. Returns
/// whether it checked at the target cadence throughout its own run (its
/// cadence, not necessarily this task's), and a note naming it.
fn cover(
    trace: &Trace,
    owner: &SpanRecord,
    start: Duration,
    len: Duration,
    options: &Options,
) -> (bool, String) {
    let end = start.saturating_add(len);
    let mut best: Option<&SpanRecord> = None;
    for s in &trace.spans {
        if s.id == owner.id
            || s.kind != SpanKind::Work
            || s.stats.checks == 0
            || s.stats.overflowed
            || s.stats.clock_regressions != 0
        {
            continue;
        }
        let overlap = s.end.min(end).saturating_sub(s.start.max(start));
        if overlap.as_nanos() * 10 >= len.as_nanos() * 9
            && best.is_none_or(|b| s.stats.max_check_gap < b.stats.max_check_gap)
        {
            best = Some(s);
        }
    }
    match best {
        // Callers pass only gaps longer than a target, so `len` is never zero.
        Some(c) => (
            c.stats.max_check_gap <= options.stop_gap_target,
            format!(
                "; task {:?} also ran across it with {} checks (longest gap {:.2} ms over its whole run)",
                c.task,
                c.stats.checks,
                ms(c.stats.max_check_gap)
            ),
        ),
        None => (false, String::new()),
    }
}

struct CallbackGroup<'a> {
    node: Option<NodeId>,
    task: &'a str,
    samples: Vec<(Duration, Duration)>,
}

/// When any work ran: the union of Work spans, merged into disjoint windows.
fn busy_windows(trace: &Trace) -> Vec<(Duration, Duration)> {
    let mut work: Vec<(Duration, Duration)> = Vec::new();
    for span in &trace.spans {
        if span.kind == SpanKind::Work {
            work.push((span.start, span.end));
        }
    }
    let mut merged: Vec<(Duration, Duration)> = Vec::new();
    for &index in &sorted_indices(work.len(), &|a, b| work[a].0.cmp(&work[b].0)) {
        let (start, end) = work[index];
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// Whether one busy window holds the whole interval from `from` to `to`.
fn within(windows: &[(Duration, Duration)], from: Duration, to: Duration) -> bool {
    for &(start, end) in windows {
        if start <= from && to <= end {
            return true;
        }
    }
    false
}

fn callback_findings(trace: &Trace, options: &Options, findings: &mut Vec<Finding>) {
    let mut groups: Vec<CallbackGroup<'_>> = Vec::new();
    'spans: for span in &trace.spans {
        if span.kind != SpanKind::Callback || span.stats.clock_regressions != 0 {
            continue;
        }
        let sample = (span.start, span.elapsed());
        for group in &mut groups {
            if group.node == span.node && group.task == span.task {
                group.samples.push(sample);
                continue 'spans;
            }
        }
        groups.push(CallbackGroup {
            node: span.node,
            task: &span.task,
            samples: vec![sample],
        });
    }
    let busy = busy_windows(trace);
    for CallbackGroup { task, samples, .. } in &groups {
        let by_start = sorted_indices(samples.len(), &|a, b| samples[a].0.cmp(&samples[b].0));
        let mut interval = Duration::ZERO;
        for i in 1..by_start.len() {
            let (from, to) = (samples[by_start[i - 1]].0, samples[by_start[i]].0);
            // Idle time between operations is not a missed callback.
            if busy.is_empty() || within(&busy, from, to) {
                interval = interval.max(to.saturating_sub(from));
            }
        }
        if interval > options.callback_interval_target {
            findings.push(finding(Kind::CallbackInterval, format!("callback {:?}: longest start-to-start interval {:.2} ms across {} retained calls (target {:.2} ms)",
                    task, ms(interval), samples.len(), ms(options.callback_interval_target)), "Poll or post notifications more regularly if the user interface needs this cadence; reporting alone does not dispatch callbacks."));
        }
        let by_duration = sorted_indices(samples.len(), &|a, b| samples[a].1.cmp(&samples[b].1));
        let maximum = samples[by_duration[samples.len() - 1]].1; // Never empty.
        if maximum <= options.callback_budget {
            continue;
        }
        let p95 = samples[by_duration[((samples.len() * 95).div_ceil(100)).saturating_sub(1)]].1;
        findings.push(finding(Kind::CallbackDuration, format!("callback {:?}: max {:.2} ms, p95 {:.2} ms across {} retained calls (budget {:.2} ms)",
                task, ms(maximum), ms(p95), samples.len(), ms(options.callback_budget)), "Keep subscriber work below the budget: defer rendering/I/O or post a coalesced notification to the owner thread. Time snapshot construction inside the callback if it happens there."));
    }
}

fn phase_spec_code(child: &Snapshot, weight: u64) -> String {
    let total = match child.initial_total {
        how_far::Total::Exact(n) => format!("Total::Exact({n})"),
        how_far::Total::Estimated(n) => format!("Total::Estimated({n})"),
        _ => "Total::Unknown".into(),
    };
    let mut code = format!("PhaseSpec::new({:?}, {weight}, {total})", child.name);
    if child.units != "items" {
        code.push_str(&format!(".units({:?})", child.units));
    }
    match child.execution {
        Execution::Unspecified => {}
        Execution::Sequence => code.push_str(".execution(Execution::Sequence)"),
        Execution::ForkJoin => code.push_str(".execution(Execution::ForkJoin)"),
        Execution::WorkPool {
            max_parallelism, ..
        } => code.push_str(&format!(
            ".execution(Execution::work_pool(NonZeroUsize::new({max_parallelism}).unwrap()))"
        )),
        _ => code.push_str(" /* preserve this stage's execution mode */"),
    }
    code
}

/// Whole-percent weights summing to 100 with every weight positive
/// (`PhaseSpec` rejects zero), distributing rounding by largest remainder.
fn percent_weights(shares: &[f64]) -> Vec<u64> {
    let mut weights = Vec::with_capacity(shares.len());
    let mut assigned = 0_u64;
    for share in shares {
        let weight = (share * 100.0).floor().max(1.0) as u64;
        weights.push(weight);
        assigned += weight;
    }
    let remainder = |i: usize| shares[i] * 100.0 - (shares[i] * 100.0).floor();
    // Largest remainder first; ties in declared order.
    let order = sorted_indices(shares.len(), &|a, b| remainder(b).total_cmp(&remainder(a)));
    while assigned < 100 {
        for &index in &order {
            if assigned == 100 {
                break;
            }
            weights[index] += 1;
            assigned += 1;
        }
    }
    while assigned > 100 {
        let before = assigned;
        for &index in order.iter().rev() {
            if assigned == 100 {
                break;
            }
            if weights[index] > 1 {
                weights[index] -= 1;
                assigned -= 1;
            }
        }
        if assigned == before {
            break; // More than 100 stages of weight 1; leave them positive.
        }
    }
    weights
}

fn stage_findings(
    trace: &Trace,
    parent: &Snapshot,
    options: &Options,
    findings: &mut Vec<Finding>,
) {
    if let Some(finding) = stage_weight_finding(trace, parent, options) {
        findings.push(finding);
    }
    for child in &parent.children {
        stage_findings(trace, child, options, findings);
    }
}

/// Weight advice for a sequence whose stages all succeeded, from one run's
/// wall time.
fn stage_weight_finding(trace: &Trace, parent: &Snapshot, options: &Options) -> Option<Finding> {
    let children = &parent.children;
    if parent.execution != Execution::Sequence || children.len() < 2 || children.len() > 100 {
        return None;
    }
    let mut durations = Vec::with_capacity(children.len());
    let mut previous_end = Duration::ZERO;
    let mut total = Duration::ZERO;
    let mut weight_sum = 0_u64;
    for child in children {
        let (start, end) = stage_time(trace, child)?;
        // Failed or overlapping stages cannot calibrate sequential weights.
        if child.status != Status::Finished(Outcome::Succeeded) || previous_end > start {
            return None;
        }
        previous_end = end;
        // A stage within one clock tick has a measured share of zero.
        let duration = end.saturating_sub(start);
        durations.push(duration);
        total = total.saturating_add(duration);
        weight_sum += child.weight;
    }
    if total < options.minimum_stage_wall || weight_sum == 0 {
        return None;
    }
    let mut shares = Vec::with_capacity(children.len());
    let mut declared = Vec::with_capacity(children.len());
    for i in 0..children.len() {
        shares.push(durations[i].as_secs_f64() / total.as_secs_f64());
        declared.push(children[i].weight as f64 / weight_sum as f64);
    }
    // A stage this small is dominated by clock and scheduling noise and may be
    // input-dependent (a flush that is trivial for this input). It keeps its
    // declared weight instead of being calibrated to a near-zero share, unless
    // the held stages together would own more than half the bar, which
    // negligible work cannot justify. Smaller planned shares are held first.
    // One bit per stage; a sequence has at most 100.
    let mut frozen = 0_u128;
    let is_frozen = |frozen: u128, index: usize| frozen >> index & 1 == 1;
    let mut frozen_mass = 0.0;
    let negligible = sorted_indices(children.len(), &|a, b| declared[a].total_cmp(&declared[b]));
    for &index in &negligible {
        if shares[index] < options.negligible_stage_share && frozen_mass + declared[index] <= 0.5 {
            frozen |= 1 << index;
            frozen_mass += declared[index];
        }
    }
    let mut free_measured = 0.0;
    for (index, share) in shares.iter().enumerate() {
        if !is_frozen(frozen, index) {
            free_measured += share;
        }
    }
    let mut candidate = Vec::with_capacity(children.len());
    let mut biggest_difference = 0.0_f64;
    for i in 0..shares.len() {
        if is_frozen(frozen, i) {
            candidate.push(declared[i]);
        } else {
            let value = (1.0 - frozen_mass) * shares[i] / free_measured;
            biggest_difference = biggest_difference.max((declared[i] - value).abs());
            candidate.push(value);
        }
    }
    let advise = free_measured > 0.0 && biggest_difference >= options.weight_difference;
    if !advise {
        return None;
    }
    let weights = percent_weights(&candidate);
    let mut held = Vec::new();
    let mut planned = Vec::with_capacity(children.len());
    let mut code = String::from("[\n");
    for (i, child) in children.iter().enumerate() {
        if is_frozen(frozen, i) {
            held.push(child.name.as_str());
        }
        planned.push(child.weight);
        code.push_str(&format!("    {},\n", phase_spec_code(child, weights[i])));
    }
    code.push(']');
    let note = if held.is_empty() {
        String::new()
    } else {
        format!(
            "; {held:?} measured under {:.1}% of the run, too small to calibrate, so it keeps its declared share",
            options.negligible_stage_share * 100.0
        )
    };
    Some(Finding {
        kind: Kind::StageWeights,
        evidence: format!(
            "sequential stage {:?}: planned {planned:?}; measured wall-time candidate {weights:?} from one run{note}",
            parent.name
        ),
        advice: "Repeat across representative inputs before changing weights. Wall time includes waits and may shift with hardware, scheduling, or configuration.".into(),
        sample_code: Some(code),
    })
}

/// Whether `span` belongs to work expected to report units: a leaf of the
/// attached tree, or any span that reported when no tree is attached. A branch
/// delegates its units, and stop-only spans have no node.
fn counts_units(trace: &Trace, span: &SpanRecord) -> bool {
    let (Some(node), Some(root)) = (span.node, &trace.progress) else {
        return span.stats.reports > 0;
    };
    find_node(root, node).is_some_and(|phase| phase.children.is_empty())
}

/// When a stage ran: from its spans' earliest start to their latest end.
/// A branch's `Wait` span covers its coordination after the split, including
/// any time after its last child finished.
fn stage_time(trace: &Trace, stage: &Snapshot) -> Option<(Duration, Duration)> {
    let mut time: Option<(Duration, Duration)> = None;
    for span in &trace.spans {
        let Some(node) = span.node else {
            continue;
        };
        if !matches!(span.kind, SpanKind::Work | SpanKind::Wait)
            || span.stats.clock_regressions != 0
            || span.outcome != Outcome::Succeeded
            || find_node(stage, node).is_none()
        {
            continue;
        }
        time = Some(match time {
            Some((start, end)) => (start.min(span.start), end.max(span.end)),
            None => (span.start, span.end),
        });
    }
    time
}

impl how_far::Complete for DiagnosticPulse {
    fn complete_as(self, outcome: Outcome) {
        let Self { inner, meter } = self;
        how_far::Complete::complete_as(inner, outcome);
        meter.close(outcome);
    }
}

fn lifecycle_findings(node: &Snapshot, parent_succeeded: bool, out: &mut Vec<Finding>) {
    let mut add = |kind, evidence: String, advice: &str| {
        out.push(finding(
            kind,
            format!("phase {:?} ({}): {}", node.name, node.id, evidence),
            advice,
        ))
    };
    if node.completion_inferred {
        add(
            Kind::InferredCompletion,
            format!("completion inferred as {:?}", node.status),
            "An unused optional phase and forgotten required work are indistinguishable from progress alone. Assert required outputs in library tests; skip explicitly when the decision is known.",
        );
    }
    if node.status == Status::Finished(Outcome::Abandoned) {
        add(
            Kind::AbandonedPhase,
            "owner or enclosing result boundary ended without this phase's result".into(),
            "Pass the final Result to complete or finish_phase. A bare ? bypasses that handoff; a panic or intentionally abandoned attempt can also cause this finding.",
        );
    }
    if parent_succeeded && node.status == Status::Finished(Outcome::Failed) {
        add(
            Kind::RecoveredFailure,
            "failed attempt inside a successful operation".into(),
            "This can be correct recovery. Keep the failed attempt visible and test that fallback fulfilled the operation's contract; the tracker cannot prove that.",
        );
    }
    if node.children.is_empty()
        && let how_far::Total::Exact(total) = node.total
        && (node.overrun()
            || (node.status == Status::Finished(Outcome::Succeeded) && node.completed != total))
    {
        add(
            Kind::CountMismatch,
            format!(
                "{} completed units for exact total {}",
                node.completed, total
            ),
            "Report completed work exactly once and flush local batches before completing the phase. If the total is not exact, declare Estimated or Unknown.",
        );
    }
    for child in &node.children {
        lifecycle_findings(
            child,
            node.status == Status::Finished(Outcome::Succeeded),
            out,
        );
    }
}

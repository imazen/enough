//! Checkpoint advice for library tests. Requires the `diagnostics` feature,
//! normally on a dev-dependency.
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
    Child, ChildPulse, Execution, NodeId, Observer, Outcome, PhaseSpec, PlanError,
    ProgressWithStop, Pulse, PulseHandle, PulseTree, Report, Snapshot, Status, Stop, StopReason,
    profile::{
        Instrumented, Profiler, SiteStats, SourceSite, Span, SpanInner, SpanKind, SpanRecord,
        Trace, advanced, checked, sorted_indices,
    },
    sync::Mutex,
};
use alloc::{boxed::Box, format, string::String, sync::Arc, vec, vec::Vec};
use core::{
    fmt,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

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
    /// may not be for the next). It keeps its declared weight and cannot
    /// trigger weight advice on its own.
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
/// use how_far_along::diagnostics::{DiagnosticPulse, Options};
/// use how_far_along::profile::{Profiler, StdClock};
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
/// [`Pulse::handle`] returns an instrumented handle, so checks made inside
/// code that owns its stop policy, such as a codec context, count toward the
/// stage that handed it out. `may_stop` and `may_report` are always `true`, so
/// libraries that skip calls on no-op pulses still make the calls measured.
/// Creating one turns on [`Profiler::set_report_timing`].
#[must_use = "finish the measured root after its work joins; dropping it unfinished records Abandoned"]
pub struct DiagnosticPulse {
    tree: PulseTree,
    meter: MeterOwner,
}

impl fmt::Debug for DiagnosticPulse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiagnosticPulse")
            .field("tree", &self.tree)
            .finish_non_exhaustive()
    }
}

impl DiagnosticPulse {
    /// Measure everything a library does through `tree`.
    pub fn new(tree: PulseTree, profiler: &Profiler) -> Self {
        profiler.set_report_timing(true);
        let observer = tree.observer();
        let snapshot = observer.snapshot();
        Self {
            meter: MeterOwner(Arc::new(Meter {
                observer,
                profiler: profiler.clone(),
                node: snapshot.id,
                name: snapshot.name,
                span: Mutex::new(None),
                entered: None,
                exited: None,
                has_children: AtomicBool::new(false),
                closed: AtomicBool::new(false),
            })),
            tree,
        }
    }

    /// A read-only view of the measured tree.
    pub fn observer(&self) -> Observer {
        self.meter.observer.clone()
    }

    /// Record the operation's outcome in the tree, and close the root's span.
    pub fn finish(self, outcome: Outcome) -> Result<(), PlanError> {
        let Self { tree, meter } = self;
        let result = tree.finish(outcome);
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
    type Target = Meter;
    fn deref(&self) -> &Meter {
        &self.0
    }
}
impl Drop for MeterOwner {
    fn drop(&mut self) {
        self.close(Outcome::Abandoned);
    }
}
struct Meter {
    observer: Observer,
    profiler: Profiler,
    node: NodeId,
    name: String,
    span: Mutex<Option<Span>>,
    /// A sequential stage: when it was entered (the previous stage's end).
    entered: Option<Arc<Mutex<Duration>>>,
    /// A sequential stage: where the next stage reads its entry time.
    exited: Option<Arc<Mutex<Duration>>>,
    /// A phase that split: its time belongs to its children's spans.
    has_children: AtomicBool,
    closed: AtomicBool,
}

impl Meter {
    fn start_span(&self) -> Span {
        match &self.entered {
            Some(entered) => {
                let at = *entered.lock();
                self.profiler
                    .span_from(Some(self.node), self.name.clone(), SpanKind::Work, at)
            }
            None => self
                .profiler
                .span(self.node, self.name.clone(), SpanKind::Work),
        }
    }

    /// This phase's span, started at its first checkpoint.
    fn span(&self) -> Arc<SpanInner> {
        let mut owner = self.span.lock();
        let span = match owner.take() {
            Some(span) => span,
            None => self.start_span(),
        };
        let shared = span.shared();
        *owner = Some(span);
        shared
    }

    fn split<'s>(
        &self,
        inner: &'s dyn Pulse,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Child<'s>>, PlanError> {
        let children = inner.split(execution, parts)?;
        self.has_children.store(true, Ordering::Relaxed);
        let snapshot = self.observer.snapshot();
        let nodes = match find_node(&snapshot, self.node) {
            Some(node) => &node.children[..],
            None => &[],
        };
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
            measured.push(Child::new(DiagnosticChild {
                inner: child,
                meter: MeterOwner(Arc::new(Meter {
                    observer: self.observer.clone(),
                    profiler: self.profiler.clone(),
                    node: match nodes.get(index) {
                        Some(node) => node.id,
                        None => self.node,
                    },
                    name: parts[index].name.into(),
                    span: Mutex::new(None),
                    entered: cells.get(index).cloned(),
                    exited: cells.get(index + 1).cloned(),
                    has_children: AtomicBool::new(false),
                    closed: AtomicBool::new(false),
                })),
            }));
        }
        Ok(measured)
    }

    fn handle(&self, inner: &dyn Pulse) -> PulseHandle {
        let handle = inner.handle();
        let span = self.span();
        ProgressWithStop::new(
            Some(Arc::new(Instrumented::new(handle.stop, Arc::clone(&span))) as Arc<dyn Stop>),
            Some(Arc::new(Instrumented::new(handle.report, span)) as Arc<dyn Report>),
        )
    }

    fn close(&self, outcome: Outcome) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut owner = self.span.lock();
        if owner.is_none()
            && outcome == Outcome::Succeeded
            && self.entered.is_some()
            && !self.has_children.load(Ordering::Relaxed)
        {
            // A leaf stage with no checkpoint still took time.
            *owner = Some(self.start_span());
        }
        if let Some(span) = owner.take() {
            span.finish(outcome);
        }
        drop(owner);
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

impl Stop for DiagnosticPulse {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        checked(&self.meter.span(), &self.tree)
    }
    fn may_stop(&self) -> bool {
        true
    }
}

impl Report for DiagnosticPulse {
    #[track_caller]
    fn advance(&self, completed: u64) {
        advanced(&self.meter.span(), &self.tree, completed);
    }
    fn may_report(&self) -> bool {
        true
    }
}

impl Pulse for DiagnosticPulse {
    fn split(
        &self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Child<'_>>, PlanError> {
        self.meter.split(&self.tree, execution, parts)
    }
    fn handle(&self) -> PulseHandle {
        self.meter.handle(&self.tree)
    }
    fn start(&self) -> Result<(), PlanError> {
        self.tree.start()
    }
    fn share(&self) -> Result<crate::SharedPulse, PlanError> {
        Ok(crate::SharedPulse::new(DiagnosticView {
            inner: self.tree.share()?,
            meter: Arc::clone(&self.meter.0),
            span: self.meter.span(),
        }))
    }
    fn set_total(&self, total: crate::Total) -> Result<(), PlanError> {
        self.tree.set_total(total)
    }
}

/// A measured child phase.
struct DiagnosticChild<'a> {
    inner: Child<'a>,
    meter: MeterOwner,
}

impl Stop for DiagnosticChild<'_> {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        checked(&self.meter.span(), &self.inner)
    }
    fn may_stop(&self) -> bool {
        true
    }
}

impl Report for DiagnosticChild<'_> {
    #[track_caller]
    fn advance(&self, completed: u64) {
        advanced(&self.meter.span(), &self.inner, completed);
    }
    fn may_report(&self) -> bool {
        true
    }
}

impl Pulse for DiagnosticChild<'_> {
    fn split(
        &self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Child<'_>>, PlanError> {
        self.meter.split(&self.inner, execution, parts)
    }
    fn handle(&self) -> PulseHandle {
        self.meter.handle(&self.inner)
    }
    fn start(&self) -> Result<(), PlanError> {
        self.inner.start()
    }
    fn share(&self) -> Result<crate::SharedPulse, PlanError> {
        Ok(crate::SharedPulse::new(DiagnosticView {
            inner: self.inner.share()?,
            meter: Arc::clone(&self.meter.0),
            span: self.meter.span(),
        }))
    }
    fn set_total(&self, total: crate::Total) -> Result<(), PlanError> {
        self.inner.set_total(total)
    }
}

impl ChildPulse for DiagnosticChild<'_> {
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

// A non-owning measurement view keeps the original span, including after close.
// SpanInner ignores late measurements; dropping this view never ends the span.
struct DiagnosticView {
    inner: crate::SharedPulse,
    meter: Arc<Meter>,
    span: Arc<SpanInner>,
}
impl Stop for DiagnosticView {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        checked(&self.span, &self.inner)
    }
    fn may_stop(&self) -> bool {
        true
    }
}
impl Report for DiagnosticView {
    #[track_caller]
    fn advance(&self, n: u64) {
        advanced(&self.span, &self.inner, n)
    }
    fn may_report(&self) -> bool {
        true
    }
}
impl Pulse for DiagnosticView {
    fn split(&self, e: Execution, parts: &[PhaseSpec<'_>]) -> Result<Vec<Child<'_>>, PlanError> {
        self.meter.split(&self.inner, e, parts)
    }
    fn handle(&self) -> PulseHandle {
        let handle = self.inner.handle();
        ProgressWithStop::new(
            Some(Arc::new(Instrumented::new(handle.stop, self.span.clone())) as Arc<dyn Stop>),
            Some(Arc::new(Instrumented::new(handle.report, self.span.clone())) as Arc<dyn Report>),
        )
    }
    fn start(&self) -> Result<(), PlanError> {
        self.inner.start()
    }
    fn set_total(&self, t: crate::Total) -> Result<(), PlanError> {
        self.inner.set_total(t)
    }
    fn share(&self) -> Result<crate::SharedPulse, PlanError> {
        Ok(crate::SharedPulse::new(Self {
            inner: self.inner.clone(),
            meter: self.meter.clone(),
            span: self.span.clone(),
        }))
    }
}

impl Trace {
    /// Analyze the retained spans and the attached progress tree, if any.
    ///
    /// Rates are per span's wall time, not exact intervals at one line.
    /// Report gaps need [`Profiler::set_report_timing`] on while recording.
    /// Callback findings need callback spans, which
    /// [`Profiler::measure_callback`] records around each callback.
    pub fn diagnose(&self, options: &Options) -> Vec<Finding> {
        let mut findings = Vec::new();
        if self.dropped_spans != 0 || self.active_spans != 0 {
            findings.push(Finding {
                kind: Kind::IncompleteEvidence,
                evidence: format!(
                    "{} spans dropped; {} still active",
                    self.dropped_spans, self.active_spans
                ),
                advice: "Increase profiler capacity and finish/join every task before diagnosing."
                    .into(),
                sample_code: None,
            });
        }
        for span in &self.spans {
            if span.stats.overflowed
                || span.stats.clock_regressions != 0
                || span.stats.unattributed_calls != 0
            {
                findings.push(Finding {
                    kind: Kind::IncompleteEvidence,
                    evidence: format!(
                        "task {:?}: overflow={}, clock regressions={}, unattributed calls={}",
                        span.task, span.stats.overflowed, span.stats.clock_regressions,
                        span.stats.unattributed_calls
                    ),
                    advice: "Use a monotonic shared clock and sufficient span/site capacity before tuning this task.".into(),
                    sample_code: None,
                });
                continue;
            }
            if span.kind != SpanKind::Work || span.outcome != Outcome::Succeeded {
                continue;
            }
            if span.stats.max_check_gap > options.stop_gap_target
                && span.elapsed() >= options.stop_gap_target
            {
                // The last site with the longest gap before a check, as
                // `max_by_key` would pick; the gap may instead end at exit.
                let mut longest: Option<&SiteStats> = None;
                for site in &span.stats.sites {
                    if longest.is_none_or(|l| site.max_gap_before_check >= l.max_gap_before_check) {
                        longest = Some(site);
                    }
                }
                let ending = match longest {
                    Some(s) if s.max_gap_before_check == span.stats.max_check_gap => {
                        format!("{}:{}:{}", s.file, s.line, s.column)
                    }
                    _ => "task exit".into(),
                };
                let cover = covering_span(
                    self,
                    span,
                    span.stats.max_check_gap_start,
                    span.stats.max_check_gap,
                );
                let covered = covers(cover, options);
                findings.push(Finding {
                    kind: Kind::StopGap,
                    evidence: format!(
                        "task {:?}: {:.2} ms without a check before {ending}{}",
                        span.task,
                        ms(span.stats.max_check_gap),
                        cover_note(cover)
                    ),
                    advice: if covered {
                        "This task's own checkpoints are sparse here, but the other task above checked at the target cadence throughout the interval. If it is the work inside this stage (for example an encoder holding its own Stop), cancellation is already covered and this is a measurement seam, not a latency problem. Otherwise add a cheap check() in the long-running section.".into()
                    } else {
                        "Add a cheap check() in the long-running section; keep the stop-only cadence independent of reporting.".into()
                    },
                    sample_code: None,
                });
            }
            if span.stats.reports > 0
                && let Some(gap) = &span.stats.max_report_gap
                && gap.duration > options.report_gap_target
            {
                let own = gap.checks > 0 && gap.max_check_gap <= options.stop_gap_target;
                let cover = if own {
                    None
                } else {
                    covering_span(self, span, gap.start, gap.duration)
                };
                let covered = covers(cover, options);
                findings.push(Finding {
                    kind: Kind::ReportGap,
                    evidence: format!(
                        "task {:?}: {:.2} ms between {} and {}; stop checks inside it: {}, longest stop gap {:.2} ms{}",
                        span.task, ms(gap.duration),
                        location(gap.from, "task entry"), location(gap.to, "task exit"),
                        gap.checks, ms(gap.max_check_gap), cover_note(cover)
                    ),
                    advice: if own || covered {
                        "Cancellation is checked at the target cadence inside this interval, so this is a progress-granularity seam rather than missing cancellation checks; more stop checks would not help. Finer progress needs a smaller reportable unit: sub-phases, an Estimated total, or a report from inside the long operation. Visible UI smoothness also depends on the consumer's polling cadence.".into()
                    } else {
                        "No stop check at the target cadence was recorded inside this interval either. Consider reporting completed units and checking cancellation here; step(n) does both.".into()
                    },
                    sample_code: None,
                });
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
                        findings.push(Finding {
                            kind,
                            evidence: format!(
                                "task {:?}: {} calls at {}:{}:{} in {:.2} ms ({:.0}/s)",
                                span.task,
                                calls,
                                site.file,
                                site.line,
                                site.column,
                                ms(span.elapsed()),
                                calls as f64 / seconds,
                            ),
                            advice: advice.into(),
                            sample_code: None,
                        });
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
/// profiler. Checks happen at that task's cadence, not necessarily this task's.
/// Among several, the first with the shortest longest gap.
fn covering_span<'a>(
    trace: &'a Trace,
    owner: &SpanRecord,
    start: Duration,
    len: Duration,
) -> Option<&'a SpanRecord> {
    if len.is_zero() {
        return None;
    }
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
    best
}
/// Whether a covering span checked at the target cadence throughout its run.
fn covers(cover: Option<&SpanRecord>, options: &Options) -> bool {
    match cover {
        Some(c) => c.stats.max_check_gap <= options.stop_gap_target,
        None => false,
    }
}
fn cover_note(cover: Option<&SpanRecord>) -> String {
    match cover {
        Some(c) => format!(
            "; task {:?} also ran across it with {} checks (longest gap {:.2} ms over its whole run)",
            c.task,
            c.stats.checks,
            ms(c.stats.max_check_gap)
        ),
        None => String::new(),
    }
}

struct CallbackGroup<'a> {
    node: Option<NodeId>,
    task: &'a str,
    samples: Vec<(Duration, Duration)>,
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
    for CallbackGroup { task, samples, .. } in &groups {
        let by_start = sorted_indices(samples.len(), &|a, b| samples[a].0.cmp(&samples[b].0));
        let mut interval: Option<Duration> = None;
        for pair in by_start.windows(2) {
            let gap = samples[pair[1]].0.saturating_sub(samples[pair[0]].0);
            interval = Some(match interval {
                Some(longest) => longest.max(gap),
                None => gap,
            });
        }
        if let Some(interval) = interval
            && interval > options.callback_interval_target
        {
            findings.push(Finding {
                kind: Kind::CallbackInterval,
                evidence: format!("callback {:?}: longest start-to-start interval {:.2} ms across {} retained calls (target {:.2} ms)",
                    task, ms(interval), samples.len(), ms(options.callback_interval_target)),
                advice: "Poll or post notifications more regularly if the user interface needs this cadence; reporting alone does not dispatch callbacks.".into(),
                sample_code: None,
            });
        }
        let by_duration = sorted_indices(samples.len(), &|a, b| samples[a].1.cmp(&samples[b].1));
        let Some(&longest) = by_duration.last() else {
            continue;
        };
        let maximum = samples[longest].1;
        if maximum <= options.callback_budget {
            continue;
        }
        let p95 = samples[by_duration[((samples.len() * 95).div_ceil(100)).saturating_sub(1)]].1;
        findings.push(Finding {
            kind: Kind::CallbackDuration,
            evidence: format!("callback {:?}: max {:.2} ms, p95 {:.2} ms across {} retained calls (budget {:.2} ms)",
                task, ms(maximum), ms(p95), samples.len(), ms(options.callback_budget)),
            advice: "Keep subscriber work below the budget: defer rendering/I/O or post a coalesced notification to the owner thread. Time snapshot construction inside the callback if it happens there.".into(),
            sample_code: None,
        });
    }
}

fn phase_spec_code(child: &Snapshot, weight: u64) -> String {
    let total = match child.initial_total {
        crate::Total::Exact(n) => format!("Total::Exact({n})"),
        crate::Total::Estimated(n) => format!("Total::Estimated({n})"),
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
    for child in children {
        if child.status != Status::Finished(Outcome::Succeeded) {
            return None;
        }
    }
    let mut durations = Vec::with_capacity(children.len());
    let mut previous_end: Option<Duration> = None;
    let mut sequential = true;
    let mut total = Duration::ZERO;
    let mut weight_sum = 0_u64;
    for child in children {
        let (start, end) = stage_time(trace, child)?;
        if previous_end.is_some_and(|previous| previous > start) {
            sequential = false;
        }
        previous_end = Some(end);
        let duration = end.saturating_sub(start);
        if duration.is_zero() {
            return None;
        }
        durations.push(duration);
        total = total.saturating_add(duration);
        weight_sum += child.weight;
    }
    if !sequential || total < options.minimum_stage_wall || weight_sum == 0 {
        return None;
    }
    let mut shares = Vec::with_capacity(children.len());
    let mut declared = Vec::with_capacity(children.len());
    let mut frozen = Vec::with_capacity(children.len());
    let mut frozen_mass = 0.0;
    let mut free_measured = 0.0;
    for (child, duration) in children.iter().zip(&durations) {
        let share = duration.as_secs_f64() / total.as_secs_f64();
        let planned = child.weight as f64 / weight_sum as f64;
        // A stage this small is dominated by clock and scheduling noise and
        // may be input-dependent (a flush that is trivial for this input). It
        // keeps its declared weight instead of being calibrated to a
        // near-zero share, unless the plan gave it most of the bar, which a
        // negligible stage cannot justify.
        let hold = share < options.negligible_stage_share && planned <= 0.5;
        if hold {
            frozen_mass += planned;
        } else {
            free_measured += share;
        }
        shares.push(share);
        declared.push(planned);
        frozen.push(hold);
    }
    let mut candidate = Vec::with_capacity(children.len());
    let mut biggest_difference = 0.0_f64;
    for i in 0..shares.len() {
        if frozen[i] {
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
        if frozen[i] {
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

/// When a stage ran: from its spans' earliest start to their latest end.
fn stage_time(trace: &Trace, stage: &Snapshot) -> Option<(Duration, Duration)> {
    let mut time: Option<(Duration, Duration)> = None;
    for span in &trace.spans {
        let Some(node) = span.node else {
            continue;
        };
        if span.kind != SpanKind::Work
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

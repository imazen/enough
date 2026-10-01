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
    profile::{Instrumented, Profiler, SourceSite, Span, SpanKind, SpanRecord, Trace},
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
    site.map_or_else(|| boundary.into(), |site| format!("{site}"))
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
pub struct DiagnosticPulse {
    tree: PulseTree,
    meter: Meter,
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
            meter: Meter {
                observer,
                profiler: profiler.clone(),
                node: snapshot.id,
                name: snapshot.name,
                span: Mutex::new(None),
                entered: None,
                exited: None,
                has_children: AtomicBool::new(false),
            },
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

    fn instrument<T>(&self, value: T) -> Instrumented<T> {
        let mut owner = self.span.lock();
        owner
            .get_or_insert_with(|| self.start_span())
            .instrument(value)
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
        let nodes = find_node(&snapshot, self.node).map(|node| &node.children);
        // cells[i] is stage i's entry and stage i-1's exit.
        let cells: Vec<_> = if execution == Execution::Sequence {
            let planned = self.profiler.now();
            (0..=children.len())
                .map(|_| Arc::new(Mutex::new(planned)))
                .collect()
        } else {
            Vec::new()
        };
        Ok(children
            .into_iter()
            .enumerate()
            .map(|(index, child)| {
                Child::new(DiagnosticChild {
                    inner: child,
                    meter: Meter {
                        observer: self.observer.clone(),
                        profiler: self.profiler.clone(),
                        node: nodes
                            .and_then(|nodes| nodes.get(index))
                            .map_or(self.node, |node| node.id),
                        name: parts[index].name.into(),
                        span: Mutex::new(None),
                        entered: cells.get(index).cloned(),
                        exited: cells.get(index + 1).cloned(),
                        has_children: AtomicBool::new(false),
                    },
                })
            })
            .collect())
    }

    fn handle(&self, inner: &dyn Pulse) -> PulseHandle {
        let handle = inner.handle();
        ProgressWithStop::new(
            Some(Arc::new(self.instrument(handle.stop)) as Arc<dyn Stop>),
            Some(Arc::new(self.instrument(handle.report)) as Arc<dyn Report>),
        )
    }

    fn close(&self, outcome: Outcome) {
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
        Some(snapshot)
    } else {
        snapshot
            .children
            .iter()
            .find_map(|child| find_node(child, id))
    }
}

fn contains_node(snapshot: &Snapshot, id: NodeId) -> bool {
    snapshot.id == id
        || snapshot
            .children
            .iter()
            .any(|child| contains_node(child, id))
}

impl Stop for DiagnosticPulse {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.meter.instrument(&self.tree).check()
    }
    fn may_stop(&self) -> bool {
        true
    }
}

impl Report for DiagnosticPulse {
    #[track_caller]
    fn advance(&self, completed: u64) {
        self.meter.instrument(&self.tree).advance(completed);
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
}

/// A measured child phase.
struct DiagnosticChild<'a> {
    inner: Child<'a>,
    meter: Meter,
}

impl Stop for DiagnosticChild<'_> {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.meter.instrument(&self.inner).check()
    }
    fn may_stop(&self) -> bool {
        true
    }
}

impl Report for DiagnosticChild<'_> {
    #[track_caller]
    fn advance(&self, completed: u64) {
        self.meter.instrument(&self.inner).advance(completed);
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
                let site = span
                    .stats
                    .sites
                    .iter()
                    .max_by_key(|s| s.max_gap_before_check)
                    .filter(|s| s.max_gap_before_check == span.stats.max_check_gap);
                let ending = site.map_or_else(
                    || "task exit".into(),
                    |s| format!("{}:{}:{}", s.file, s.line, s.column),
                );
                let cover = covering_span(
                    self,
                    span,
                    span.stats.max_check_gap_start,
                    span.stats.max_check_gap,
                );
                let covered =
                    cover.is_some_and(|c| c.stats.max_check_gap <= options.stop_gap_target);
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
                let covered =
                    cover.is_some_and(|c| c.stats.max_check_gap <= options.stop_gap_target);
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
                for (kind, threshold, advice) in [
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
                    let mut sites: Vec<_> = span.stats.sites.iter().collect();
                    sites.sort_unstable_by_key(|s| {
                        core::cmp::Reverse(match kind {
                            Kind::CheckFrequency => s.checks,
                            _ => s.reports,
                        })
                    });
                    for site in sites.into_iter().take(3) {
                        let calls = match kind {
                            Kind::CheckFrequency => site.checks,
                            _ => site.reports,
                        };
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
    trace
        .spans
        .iter()
        .filter(|s| {
            s.id != owner.id
                && s.kind == SpanKind::Work
                && s.stats.checks > 0
                && !s.stats.overflowed
                && s.stats.clock_regressions == 0
        })
        .map(|s| (s, s.end.min(end).saturating_sub(s.start.max(start))))
        .filter(|(_, overlap)| overlap.as_nanos() * 10 >= len.as_nanos() * 9)
        .min_by_key(|(s, _)| s.stats.max_check_gap)
        .map(|(s, _)| s)
}
fn cover_note(cover: Option<&SpanRecord>) -> String {
    cover.map_or_else(String::new, |c| {
        format!(
            "; task {:?} also ran across it with {} checks (longest gap {:.2} ms over its whole run)",
            c.task,
            c.stats.checks,
            ms(c.stats.max_check_gap)
        )
    })
}

struct CallbackGroup<'a> {
    node: Option<NodeId>,
    task: &'a str,
    samples: Vec<(Duration, Duration)>,
}

fn callback_findings(trace: &Trace, options: &Options, findings: &mut Vec<Finding>) {
    let mut groups: Vec<CallbackGroup<'_>> = Vec::new();
    for span in &trace.spans {
        if span.kind != SpanKind::Callback || span.stats.clock_regressions != 0 {
            continue;
        }
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.node == span.node && group.task == span.task)
        {
            group.samples.push((span.start, span.elapsed()));
        } else {
            groups.push(CallbackGroup {
                node: span.node,
                task: &span.task,
                samples: vec![(span.start, span.elapsed())],
            });
        }
    }
    for CallbackGroup {
        task, mut samples, ..
    } in groups
    {
        samples.sort_unstable_by_key(|(start, _)| *start);
        if let Some(interval) = samples
            .windows(2)
            .map(|pair| pair[1].0.saturating_sub(pair[0].0))
            .max()
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
        let mut durations: Vec<_> = samples.iter().map(|(_, duration)| *duration).collect();
        durations.sort_unstable();
        let maximum = *durations.last().unwrap();
        if maximum <= options.callback_budget {
            continue;
        }
        let p95 = durations[((durations.len() * 95).div_ceil(100)).saturating_sub(1)];
        findings.push(Finding {
            kind: Kind::CallbackDuration,
            evidence: format!("callback {:?}: max {:.2} ms, p95 {:.2} ms across {} retained calls (budget {:.2} ms)",
                task, ms(maximum), ms(p95), durations.len(), ms(options.callback_budget)),
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
    let mut weights: Vec<u64> = shares
        .iter()
        .map(|share| (share * 100.0).floor().max(1.0) as u64)
        .collect();
    let mut assigned = weights.iter().sum::<u64>();
    let mut order: Vec<usize> = (0..weights.len()).collect();
    order.sort_unstable_by(|&a, &b| {
        (shares[b] * 100.0 - (shares[b] * 100.0).floor())
            .total_cmp(&(shares[a] * 100.0 - (shares[a] * 100.0).floor()))
    });
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
    if parent.execution == Execution::Sequence
        && parent.children.len() >= 2
        && parent.children.len() <= 100
        && parent
            .children
            .iter()
            .all(|c| c.status == Status::Finished(Outcome::Succeeded))
    {
        let times: Option<Vec<(Duration, Duration)>> = parent
            .children
            .iter()
            .map(|child| {
                let mut spans = trace.spans.iter().filter(|s| {
                    s.node.is_some_and(|node| contains_node(child, node))
                        && s.kind == SpanKind::Work
                        && s.stats.clock_regressions == 0
                        && s.outcome == Outcome::Succeeded
                });
                let first = spans.next()?;
                let mut start = first.start;
                let mut end = first.end;
                for span in spans {
                    start = start.min(span.start);
                    end = end.max(span.end);
                }
                Some((start, end))
            })
            .collect();
        if let Some(times) = times {
            let sequential = times.windows(2).all(|pair| pair[0].1 <= pair[1].0);
            let durations: Vec<_> = times
                .iter()
                .map(|(start, end)| end.saturating_sub(*start))
                .collect();
            let total = durations
                .iter()
                .fold(Duration::ZERO, |sum, d| sum.saturating_add(*d));
            let weight_sum: u64 = parent.children.iter().map(|c| c.weight).sum();
            if sequential
                && total >= options.minimum_stage_wall
                && weight_sum > 0
                && durations.iter().all(|d| !d.is_zero())
            {
                let shares: Vec<f64> = durations
                    .iter()
                    .map(|d| d.as_secs_f64() / total.as_secs_f64())
                    .collect();
                let declared: Vec<f64> = parent
                    .children
                    .iter()
                    .map(|child| child.weight as f64 / weight_sum as f64)
                    .collect();
                // A stage this small is dominated by clock and scheduling noise
                // and may be input-dependent (a flush that is trivial for this
                // input). It keeps its declared weight instead of being
                // calibrated to a near-zero share, unless the plan gave it
                // most of the bar, which a negligible stage cannot justify.
                let frozen: Vec<bool> = shares
                    .iter()
                    .zip(&declared)
                    .map(|(share, planned)| {
                        *share < options.negligible_stage_share && *planned <= 0.5
                    })
                    .collect();
                let frozen_mass: f64 = declared
                    .iter()
                    .zip(&frozen)
                    .filter_map(|(planned, frozen)| frozen.then_some(*planned))
                    .sum();
                let free_measured: f64 = shares
                    .iter()
                    .zip(&frozen)
                    .filter_map(|(share, frozen)| (!frozen).then_some(*share))
                    .sum();
                let candidate: Vec<f64> = (0..shares.len())
                    .map(|i| {
                        if frozen[i] {
                            declared[i]
                        } else {
                            (1.0 - frozen_mass) * shares[i] / free_measured
                        }
                    })
                    .collect();
                let biggest_difference = (0..shares.len())
                    .filter(|i| !frozen[*i])
                    .map(|i| (declared[i] - candidate[i]).abs())
                    .fold(0.0_f64, f64::max);
                if free_measured > 0.0 && biggest_difference >= options.weight_difference {
                    let weights = percent_weights(&candidate);
                    let held: Vec<_> = parent
                        .children
                        .iter()
                        .zip(&frozen)
                        .filter_map(|(child, frozen)| frozen.then_some(child.name.as_str()))
                        .collect();
                    let note = if held.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "; {held:?} measured under {:.1}% of the run, too small to calibrate, so it keeps its declared share",
                            options.negligible_stage_share * 100.0
                        )
                    };
                    let mut code = String::from("[\n");
                    for (child, weight) in parent.children.iter().zip(&weights) {
                        code.push_str(&format!("    {},\n", phase_spec_code(child, *weight)));
                    }
                    code.push(']');
                    findings.push(Finding {
                        kind: Kind::StageWeights,
                        evidence: format!("sequential stage {:?}: planned {:?}; measured wall-time candidate {:?} from one run{note}",
                            parent.name, parent.children.iter().map(|c| c.weight).collect::<Vec<_>>(), weights),
                        advice: "Repeat across representative inputs before changing weights. Wall time includes waits and may shift with hardware, scheduling, or configuration.".into(),
                        sample_code: Some(code),
                    });
                }
            }
        }
    }
    for child in &parent.children {
        stage_findings(trace, child, options, findings);
    }
}

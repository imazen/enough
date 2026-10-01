//! Opt-in, evidence-based suggestions for library and application tests.
//!
//! Enable `diagnostics` on a **dev-dependency**, instrument each logical task
//! with [`crate::profile::Span::instrument`], and inspect the completed trace.
//! The same wrapper accepts an `enough::Stop` even when no progress is used.
//! These are heuristics: clock reads and bookkeeping affect the measured run,
//! and a single run cannot establish optimal weights or production latency.

use crate::{
    Execution, Observer, Outcome, PhaseSpec, PlanError, Pulse, Report, Snapshot, Status, Stop,
    StopReason,
    profile::{Instrumented, Profiler, SourceSite, Span, SpanKind, SpanRecord, Trace},
    sync::Mutex,
};
use alloc::{boxed::Box, format, string::String, sync::Arc, vec, vec::Vec};
use core::{
    fmt,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

/// Thresholds for a diagnostic pass. Edit fields after [`Default::default`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Options {
    /// Desired upper bound for a task's interval without a stop check.
    pub stop_gap_target: Duration,
    /// Desired upper bound for a task's interval without a progress report.
    pub report_gap_target: Duration,
    /// Maximum acceptable subscriber invocation time; defaults to 10 ms.
    pub callback_budget: Duration,
    /// Desired maximum time between successive invocations of one subscriber.
    pub callback_interval_target: Duration,
    /// Rate above which a heavily used check site gets a call-frequency hint.
    pub check_calls_per_second: f64,
    /// Rate above which a heavily used report site gets a batching hint.
    pub report_calls_per_second: f64,
    /// Minimum calls before a frequency hint is emitted.
    pub minimum_calls: u64,
    /// Minimum sequential stage wall time before proposing different weights.
    pub minimum_stage_wall: Duration,
    /// Minimum absolute difference between planned and measured child shares.
    pub weight_difference: f64,
    /// A sequential stage measured below this share of the stages' total wall
    /// time is too small to calibrate from one run: it keeps its declared
    /// weight and cannot by itself trigger weight advice.
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

/// The kind of evidence behind one suggestion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
    /// A task went too long without checking cancellation.
    StopGap,
    /// A task went too long between reports.
    ReportGap,
    /// A check call site ran at a high measured frequency.
    CheckFrequency,
    /// A reporting call site ran at a high measured frequency.
    ReportFrequency,
    /// A measured subscriber invocation exceeded its budget.
    CallbackDuration,
    /// Successive subscriber invocations were farther apart than the target.
    CallbackInterval,
    /// Measured sequential stage times differed substantially from weights.
    StageWeights,
    /// Retention, overflow, or clock quality makes guidance incomplete.
    IncompleteEvidence,
}

/// One diagnostic finding with its measurement and an actionable suggestion.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Finding {
    /// Finding category.
    pub kind: Kind,
    /// Measured evidence, including task and source locations when available.
    pub evidence: String,
    /// Suggested change or further measurement.
    pub advice: String,
    /// Copyable Rust sketch when the finding concerns stage declarations.
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
    site.map_or_else(
        || boundary.into(),
        |s| format!("{}:{}:{}", s.file, s.line, s.column),
    )
}

enum PulseRef<'a> {
    Borrowed(&'a dyn Pulse),
    Owned(Box<dyn Pulse + 'a>),
}
impl PulseRef<'_> {
    fn as_pulse(&self) -> &dyn Pulse {
        match self {
            Self::Borrowed(pulse) => *pulse,
            Self::Owned(pulse) => pulse.as_ref(),
        }
    }
}

/// Dev-only wrapper around a library's ordinary `&dyn Pulse` call.
///
/// Child phases get separate spans with their actual progress-tree node IDs.
/// A span starts at the first check or report. A stage of a *sequential* split
/// instead starts when the previous stage finished (the first one when the
/// plan was made) and always gets a span when it succeeds, so work before its
/// first checkpoint, or a stage with no checkpoint at all, is still timed.
/// A phase shared by workers still has aggregate timing; instrument each
/// worker separately when its own long tail matters.
///
/// `may_stop` and `may_report` are `true` even over a no-op pulse, so a library
/// that skips checkpoints behind them still shows its real call sites.
pub struct DiagnosticPulse<'a> {
    inner: PulseRef<'a>,
    observer: Observer,
    profiler: Profiler,
    node: usize,
    name: String,
    span: Mutex<Option<Span>>,
    /// Sequential stage: shared cell holding when this stage was entered.
    entered: Option<Arc<Mutex<Duration>>>,
    /// Sequential stage: shared cell the next stage reads as its entry time.
    exited: Option<Arc<Mutex<Duration>>>,
    /// A container stage's time belongs to its children's spans.
    has_children: AtomicBool,
}
impl<'a> DiagnosticPulse<'a> {
    /// Wrap a pulse and its matching observer without changing library code.
    pub fn new(pulse: &'a dyn Pulse, observer: Observer, profiler: &Profiler) -> Self {
        let snapshot = observer.snapshot();
        Self {
            inner: PulseRef::Borrowed(pulse),
            observer,
            profiler: profiler.clone(),
            node: snapshot.id,
            name: snapshot.name,
            span: Mutex::new(None),
            entered: None,
            exited: None,
            has_children: AtomicBool::new(false),
        }
    }
    fn start_span(&self) -> Span {
        match &self.entered {
            Some(entered) => {
                let at = *entered.lock();
                self.profiler
                    .span_from(self.node, self.name.clone(), SpanKind::Work, at)
            }
            None => self
                .profiler
                .span(self.node, self.name.clone(), SpanKind::Work),
        }
    }
    fn meter(&self) -> Instrumented<&dyn Pulse> {
        let mut owner = self.span.lock();
        let span = owner.get_or_insert_with(|| self.start_span());
        span.instrument(self.inner.as_pulse())
    }
}
fn find_node(snapshot: &Snapshot, id: usize) -> Option<&Snapshot> {
    if snapshot.id == id {
        Some(snapshot)
    } else {
        snapshot
            .children
            .iter()
            .find_map(|child| find_node(child, id))
    }
}
fn contains_node(snapshot: &Snapshot, id: usize) -> bool {
    snapshot.id == id
        || snapshot
            .children
            .iter()
            .any(|child| contains_node(child, id))
}
impl Stop for DiagnosticPulse<'_> {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.meter().check()
    }
    fn may_stop(&self) -> bool {
        true
    }
}
impl Report for DiagnosticPulse<'_> {
    #[track_caller]
    fn advance(&self, completed: u64) {
        self.meter().advance(completed);
    }
    fn may_report(&self) -> bool {
        true
    }
}
impl Pulse for DiagnosticPulse<'_> {
    fn split(
        &self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Box<dyn Pulse + '_>>, PlanError> {
        let children = self.inner.as_pulse().split(execution, parts)?;
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
                Box::new(DiagnosticPulse {
                    inner: PulseRef::Owned(child),
                    observer: self.observer.clone(),
                    profiler: self.profiler.clone(),
                    node: nodes.and_then(|n| n.get(index)).map_or(self.node, |n| n.id),
                    name: parts[index].name.into(),
                    span: Mutex::new(None),
                    entered: cells.get(index).cloned(),
                    exited: cells.get(index + 1).cloned(),
                    has_children: AtomicBool::new(false),
                }) as Box<dyn Pulse>
            })
            .collect())
    }
    fn finish(&self, outcome: Outcome) -> Result<(), PlanError> {
        self.inner.as_pulse().finish(outcome)?;
        let mut owner = self.span.lock();
        if owner.is_none()
            && outcome == Outcome::Succeeded
            && self.entered.is_some()
            && !self.has_children.load(Ordering::Relaxed)
        {
            // A leaf stage with no checkpoint still occupied wall time.
            *owner = Some(self.start_span());
        }
        if let Some(span) = owner.take() {
            span.finish(outcome);
        }
        drop(owner);
        if let Some(exited) = &self.exited {
            // Read after the span closed so the next stage never starts before this one ends.
            *exited.lock() = self.profiler.now();
        }
        Ok(())
    }
}

impl Trace {
    /// Analyze retained spans and an optional attached progress tree.
    ///
    /// Frequencies are per task wall time, not exact intervals at one line.
    /// Report gaps require this feature at recording time. Callback guidance
    /// requires callback spans; [`crate::profile::Profiler::measure_callback`]
    /// wraps one subscriber invocation without changing the poller's API.
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
    node: usize,
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
        Execution::WorkPool { max_parallelism } => code.push_str(&format!(
            ".execution(Execution::WorkPool {{ max_parallelism: {max_parallelism} }})"
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
                    contains_node(child, s.node)
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

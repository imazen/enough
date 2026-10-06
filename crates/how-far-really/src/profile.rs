//! Measure how often work checks for cancellation and reports progress.
//!
//! Create one [`Span`] per logical task or chunk, then wrap that task's stop
//! policy or progress sink with [`Span::instrument`]. The wrapper records every
//! check and report at its original call site. One span per task matters: a
//! single span shared by a pool would let one busy worker hide another
//! worker's long silence. Separate span kinds record queueing, joins, host
//! suspensions, and callbacks.
//!
//! Durations are wall time, **not CPU time**, read from the [`Clock`] you
//! supply. Retention is bounded, and anything dropped is counted in every
//! export. The collector uses OS mutexes, so it requires `std`; it belongs on
//! native threads or browser workers. Browser UI readers use
//! [`Profiler::try_snapshot`].
//!
//! # Stability
//!
//! The record types ([`Trace`], [`SpanRecord`], [`Stats`], [`SiteStats`],
//! [`ReportGap`], ...) are `#[non_exhaustive]` with public fields. Later
//! releases may add fields but will not rename, retype or remove them. JSON
//! from [`Trace::write_json`] follows the same rule: readers must ignore keys
//! they do not know, and `schema_version` changes only if a key is removed or
//! its meaning changes.

use crate::json::{outcome_name, quote, stop_reason_name};
use crate::sync::Mutex;
use alloc::{string::String, sync::Arc, vec::Vec};
use core::{
    fmt,
    panic::Location,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};
use how_far::{Outcome, Report, Stop, StopReason};
use how_far_along::NodeId;

/// A monotonic clock with one shared epoch. Only instrumented calls read it.
///
/// Browser hosts implement it over `performance.now()`. Methods added in
/// later compatible releases will have default bodies.
pub trait Clock: Send + Sync {
    /// Time elapsed since the clock's epoch.
    fn now(&self) -> Duration;
}
impl<C: Clock + ?Sized> Clock for Arc<C> {
    fn now(&self) -> Duration {
        (**self).now()
    }
}

/// The host's monotonic clock, with its epoch at construction.
#[derive(Clone, Copy, Debug)]
pub struct StdClock(std::time::Instant);
impl StdClock {
    /// Start an epoch now.
    pub fn new() -> Self {
        Self(std::time::Instant::now())
    }
}
impl Default for StdClock {
    fn default() -> Self {
        Self::new()
    }
}
impl Clock for StdClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

/// What a span measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SpanKind {
    /// A logical task's work, including any nested waits or callbacks.
    Work,
    /// Work waiting in a queue before a worker claims it.
    Queued,
    /// Waiting, such as a coordinator's join.
    Wait,
    /// A host suspension, such as a JSPI await.
    Yield,
    /// An observer or application callback.
    Callback,
}

impl SpanKind {
    fn name(self) -> &'static str {
        match self {
            Self::Work => "Work",
            Self::Queued => "Queued",
            Self::Wait => "Wait",
            Self::Yield => "Yield",
            Self::Callback => "Callback",
        }
    }
}

/// A span's identity within one profiler.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpanId(usize);

impl SpanId {
    /// The identifier as a number, as written in JSON exports.
    pub const fn get(self) -> usize {
        self.0
    }
}

impl fmt::Display for SpanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A source location captured by an instrumented call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct SourceSite {
    /// Source file.
    pub file: &'static str,
    /// Line.
    pub line: u32,
    /// Column.
    pub column: u32,
}

impl SourceSite {
    pub(crate) fn from_location(at: &'static Location<'static>) -> Self {
        Self {
            file: at.file(),
            line: at.line(),
            column: at.column(),
        }
    }
}

impl fmt::Display for SourceSite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.column)
    }
}

/// Counts for one call site within one span.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct SiteStats {
    /// Source file, captured through `#[track_caller]`.
    pub file: &'static str,
    /// Line.
    pub line: u32,
    /// Column.
    pub column: u32,
    /// Cancellation checks.
    pub checks: u64,
    /// Report calls.
    pub reports: u64,
    /// Units those reports added (saturating).
    pub units: u64,
    /// Longest interval that ended at a check at this site.
    pub max_gap_before_check: Duration,
}

/// The longest interval between reports in one span, recorded only when
/// [`Profiler::set_report_timing`] is on.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ReportGap {
    /// Start of the interval, in the clock's epoch.
    pub start: Duration,
    /// Length of the interval.
    pub duration: Duration,
    /// Cancellation checks this span made inside the interval.
    pub checks: u64,
    /// Longest interval without a check inside it, boundaries included.
    pub max_check_gap: Duration,
    /// The report that opened the interval; `None` for span entry.
    pub from: Option<SourceSite>,
    /// The report that closed the interval; `None` for span exit.
    pub to: Option<SourceSite>,
}

/// One span's checkpoint evidence. Gaps include entry and exit, so a span
/// with no checks still records how long it ran unchecked.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Stats {
    /// Cancellation checks (saturating).
    pub checks: u64,
    /// Report calls (saturating).
    pub reports: u64,
    /// Units reported (saturating).
    pub units: u64,
    /// Whether any count saturated; rates then lose their meaning.
    pub overflowed: bool,
    /// Longest interval without a check: entry to first check, check to
    /// check, or last check to exit.
    pub max_check_gap: Duration,
    /// Start of that interval, in the clock's epoch.
    pub max_check_gap_start: Duration,
    /// Longest interval between reports, if report timing was on.
    pub max_report_gap: Option<ReportGap>,
    /// Time spent inside instrumented checks, including any work they ran.
    ///
    /// Timing a check takes a second clock read, so a span times its first
    /// 16 checks, every 16th after them, and every check that returned a
    /// stop, and scales the timed total to all its checks. A slow check
    /// that was not timed still shows in [`max_check_gap`](Self::max_check_gap),
    /// which runs from one check's start to the next.
    pub check_time: Duration,
    /// When the first check that returned a stop error finished.
    pub stopped_at: Option<Duration>,
    /// That check's reason, which keeps cancellation apart from a timeout.
    pub stop_reason: Option<StopReason>,
    /// Times the clock went backwards within one call, or at finish. Workers
    /// sharing a span can deliver readings out of order; that is expected,
    /// counts as a zero gap, and is not a regression.
    pub clock_regressions: u64,
    /// Up to 64 distinct call sites.
    pub sites: Vec<SiteStats>,
    /// Calls from sites beyond the first 64. The totals above still count them.
    pub unattributed_calls: u64,
}

/// One finished span.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct SpanRecord {
    /// This span's identity.
    pub id: SpanId,
    /// The enclosing span, for spans made with [`Span::child`].
    pub parent: Option<SpanId>,
    /// The progress-tree phase this span belongs to, if any.
    pub node: Option<NodeId>,
    /// The task's label: a logical task, chunk or attempt, not an OS thread.
    pub task: String,
    /// What the span measures.
    pub kind: SpanKind,
    /// Start, in the clock's epoch.
    pub start: Duration,
    /// End, in the clock's epoch.
    pub end: Duration,
    /// How the span ended; a dropped span records `Abandoned`.
    pub outcome: Outcome,
    /// Checkpoint evidence.
    pub stats: Stats,
}

impl SpanRecord {
    /// Wall time from start to end, including nested spans. Do not add a
    /// parent's time to its children's.
    pub fn elapsed(&self) -> Duration {
        self.end.saturating_sub(self.start)
    }
    /// Checks per second of this span's wall time.
    pub fn checks_per_second(&self) -> f64 {
        if self.elapsed().is_zero() {
            0.0
        } else {
            self.stats.checks as f64 / self.elapsed().as_secs_f64()
        }
    }
    /// Whether this span made at least `minimum_checks` checks at more than
    /// `checks_per_second`. A high rate alone does not prove waste.
    pub fn is_poll_storm(&self, minimum_checks: u64, checks_per_second: f64) -> bool {
        !self.stats.overflowed
            && self.stats.checks >= minimum_checks
            && self.checks_per_second() > checks_per_second
    }
}

struct Inner {
    clock: Arc<dyn Clock>,
    capacity: usize,
    report_timing: AtomicBool,
    next_id: AtomicUsize,
    active: AtomicUsize,
    state: Mutex<Trace>,
}

/// A bounded, cloneable span collector. It starts no threads, keeps no global
/// state, and reads only the clock you give it.
#[derive(Clone)]
pub struct Profiler {
    inner: Arc<Inner>,
}

impl fmt::Debug for Profiler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Profiler")
            .field("capacity", &self.inner.capacity)
            .finish_non_exhaustive()
    }
}

impl Profiler {
    pub(crate) fn incident(&self, incident: crate::diagnostics::Incident) {
        let mut state = self.inner.state.lock();
        if state.incidents.len() < self.inner.capacity {
            state.incidents.push(incident);
        } else {
            state.dropped_incidents = state.dropped_incidents.saturating_add(1);
        }
    }

    /// Keep up to `capacity` finished spans. Later spans are dropped and
    /// counted, so every export shows incomplete coverage.
    pub fn new(clock: impl Clock + 'static, capacity: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                clock: Arc::new(clock),
                capacity,
                report_timing: AtomicBool::new(false),
                next_id: AtomicUsize::new(0),
                active: AtomicUsize::new(0),
                state: Mutex::new(Trace {
                    schema_version: 2,
                    incidents: Vec::new(),
                    dropped_incidents: 0,
                    progress: None,
                    metadata: Vec::new(),
                    spans: Vec::new(),
                    dropped_spans: 0,
                    active_spans: 0,
                    cancelled_at: None,
                    observed_at: None,
                    returned_at: None,
                }),
            }),
        }
    }

    /// Time reports as well as checks, in spans started from now on.
    ///
    /// Off by default because it reads the clock at every instrumented report.
    /// With it on, each span records its longest [`ReportGap`].
    /// [`DiagnosticPulse`](crate::diagnostics::DiagnosticPulse) turns it on.
    pub fn set_report_timing(&self, enabled: bool) {
        self.inner.report_timing.store(enabled, Ordering::Relaxed);
    }

    /// Record a key/value pair, such as a configuration or build identity.
    /// A repeated key replaces the earlier value.
    pub fn metadata(&self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        let mut state = self.inner.state.lock();
        if let Some((_, old)) = state.metadata.iter_mut().find(|(k, _)| *k == key) {
            *old = value;
        } else {
            state.metadata.push((key, value));
        }
    }

    /// Start a span for one task. `node` ties it to a progress-tree phase; pass
    /// `None` for work outside a tree.
    ///
    /// ```
    /// use how_far_along::{Outcome, Unstoppable};
    /// use how_far_really::profile::{Profiler, SpanKind, StdClock};
    ///
    /// let profiler = Profiler::new(StdClock::new(), 16);
    /// let span = profiler.span(None, "decode", SpanKind::Work);
    /// let stop = span.instrument(Unstoppable);
    /// // decode(&input, &stop)?;
    /// span.finish(Outcome::Succeeded);
    /// assert_eq!(profiler.snapshot().spans.len(), 1);
    /// ```
    pub fn span(
        &self,
        node: impl Into<Option<NodeId>>,
        task: impl Into<String>,
        kind: SpanKind,
    ) -> Span {
        self.start_span(None, node.into(), task.into(), kind, None)
    }

    /// Run one callback inside a `Callback` span. A panic records the span as
    /// abandoned and keeps unwinding.
    pub fn measure_callback<R>(&self, task: impl Into<String>, callback: impl FnOnce() -> R) -> R {
        let span = self.span(None, task, SpanKind::Callback);
        let result = callback();
        span.finish(Outcome::Succeeded);
        result
    }

    /// The clock's current reading.
    pub(crate) fn now(&self) -> Duration {
        self.inner.clock.now()
    }

    /// `entered`, if known, is when the span began, before its first checkpoint.
    #[allow(deprecated)] // Atomic::try_update is newer than the Rust 1.88 MSRV.
    pub(crate) fn start_span(
        &self,
        parent: Option<SpanId>,
        node: Option<NodeId>,
        task: String,
        kind: SpanKind,
        entered: Option<Duration>,
    ) -> Span {
        // Saturates: a span past the last identifier is counted as dropped,
        // never a panic inside an instrumented check.
        let id = self
            .inner
            .next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                Some(n.saturating_add(1))
            });
        let (Ok(id) | Err(id)) = id;
        let now = self.inner.clock.now();
        let start = entered.unwrap_or(now).min(now);
        self.inner.active.fetch_add(1, Ordering::Relaxed);
        Span {
            finished: false,
            inner: Arc::new(SpanInner {
                profiler: self.clone(),
                id: SpanId(id),
                parent,
                node,
                task,
                kind,
                start,
                time_reports: self.inner.report_timing.load(Ordering::Relaxed),
                checks_started: AtomicUsize::new(0),
                state: Mutex::new(SpanState::new(nanos(start))),
            }),
        }
    }

    /// Record when cancellation was actually requested. Call it next to the
    /// application's cancel call, not when a callback notices.
    pub fn cancellation_requested(&self) {
        let now = self.inner.clock.now();
        let mut state = self.inner.state.lock();
        state.cancelled_at = Some(state.cancelled_at.unwrap_or(now).min(now));
    }

    /// Record when the operation returned, after every worker joined and
    /// cleanup finished.
    pub fn operation_returned(&self) {
        let now = self.inner.clock.now();
        self.inner.state.lock().returned_at = Some(now);
    }

    /// Copy the finished spans. Active and dropped counts show what is missing.
    pub fn snapshot(&self) -> Trace {
        let state = self.inner.state.lock();
        self.snapshot_from(&state)
    }

    /// [`snapshot`](Self::snapshot) without waiting: `None` if another thread
    /// holds the collector's lock. UI threads keep their last trace and retry.
    pub fn try_snapshot(&self) -> Option<Trace> {
        let state = self.inner.state.try_lock()?;
        Some(self.snapshot_from(&state))
    }

    fn snapshot_from(&self, state: &Trace) -> Trace {
        let mut trace = state.clone();
        trace.active_spans = self.inner.active.load(Ordering::Relaxed);
        trace
    }
}

/// A reading in nanoseconds of the clock's epoch: hot state is kept in
/// these, which compare and subtract in one instruction, and `finish` turns
/// them into the public `Duration`s. Exact below 584 years.
fn nanos(at: Duration) -> u64 {
    at.as_secs()
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::from(at.subsec_nanos()))
}

struct SpanState {
    stats: Stats,
    /// Checks whose duration `check_time` sums so far.
    timed_checks: u64,
    check_time: u64,
    max_check_gap: u64,
    max_check_gap_start: u64,
    /// `stats.max_report_gap`'s duration.
    max_report_gap: Option<u64>,
    last_check: u64,
    last_report: u64,
    last_report_site: Option<SourceSite>,
    /// Checks since the last report, to tell a coarse reporting unit from
    /// missing cancellation checks.
    window: Window,
    /// `stats.sites[i].max_gap_before_check`.
    site_gaps: Vec<u64>,
    /// Indices in `stats.sites` of the last check's and the last report's
    /// sites, tried first: a loop that steps alternates between the two.
    check_site: usize,
    report_site: usize,
    closed: bool,
}

impl SpanState {
    fn new(start: u64) -> Self {
        Self {
            stats: Stats::default(),
            timed_checks: 0,
            check_time: 0,
            max_check_gap: 0,
            max_check_gap_start: 0,
            max_report_gap: None,
            last_check: start,
            last_report: start,
            last_report_site: None,
            window: Window::new(start),
            site_gaps: Vec::new(),
            check_site: 0,
            report_site: 0,
            closed: false,
        }
    }

    /// End the interval since the last report at `end`, keeping it if longest.
    fn close_report_gap(&mut self, end: u64, duration: u64, to: Option<SourceSite>) {
        let (checks, max_check_gap) = self.window.close(end);
        if self.max_report_gap.is_none_or(|old| duration > old) {
            self.max_report_gap = Some(duration);
            self.stats.max_report_gap = Some(ReportGap {
                start: Duration::from_nanos(self.last_report),
                duration: Duration::from_nanos(duration),
                checks,
                max_check_gap: Duration::from_nanos(max_check_gap),
                from: self.last_report_site,
                to,
            });
        }
    }

    /// The statistics as of now, with the timed checks' total scaled to all
    /// of them; see [`Stats::check_time`].
    fn take_stats(&mut self) -> Stats {
        let (timed, checks) = (self.timed_checks, self.stats.checks);
        let check_time = if timed != 0 && timed < checks {
            u128::from(self.check_time) * u128::from(checks) / u128::from(timed)
        } else {
            u128::from(self.check_time)
        };
        self.stats.check_time = Duration::from_nanos(u64::try_from(check_time).unwrap_or(u64::MAX));
        self.stats.max_check_gap = Duration::from_nanos(self.max_check_gap);
        self.stats.max_check_gap_start = Duration::from_nanos(self.max_check_gap_start);
        for (site, &gap) in self.stats.sites.iter_mut().zip(&self.site_gaps) {
            site.max_gap_before_check = Duration::from_nanos(gap);
        }
        core::mem::take(&mut self.stats)
    }
}

struct Window {
    last_check: u64,
    checks: u64,
    max_gap: u64,
}

impl Window {
    fn new(at: u64) -> Self {
        Self {
            last_check: at,
            checks: 0,
            max_gap: 0,
        }
    }
    fn check(&mut self, at: u64) {
        self.max_gap = self.max_gap.max(at.saturating_sub(self.last_check));
        self.last_check = self.last_check.max(at);
        self.checks = self.checks.saturating_add(1);
    }
    /// Close the interval at `end` and start the next one there.
    fn close(&mut self, end: u64) -> (u64, u64) {
        let gap = self.max_gap.max(end.saturating_sub(self.last_check));
        let checks = self.checks;
        *self = Self::new(end);
        (checks, gap)
    }
}

pub(crate) struct SpanInner {
    profiler: Profiler,
    id: SpanId,
    parent: Option<SpanId>,
    node: Option<NodeId>,
    task: String,
    kind: SpanKind,
    start: Duration,
    time_reports: bool,
    /// Checks started, to choose which ones to time without the lock. A wrap
    /// only times a few more.
    checks_started: AtomicUsize,
    state: Mutex<SpanState>,
}

/// A span times its first this-many checks, then every this-many-th.
const TIMED_CHECKS: usize = 16;

impl SpanInner {
    fn now(&self) -> Duration {
        self.profiler.inner.clock.now()
    }

    /// Whether to time the check about to start: see [`Stats::check_time`].
    fn time_next_check(&self) -> bool {
        let n = self.checks_started.fetch_add(1, Ordering::Relaxed);
        n < TIMED_CHECKS || n % TIMED_CHECKS == 0
    }

    /// Record a check that started at `start` and just returned `result`,
    /// at `end` if it was timed. Shared by every [`Instrumented`] type, so it
    /// is compiled once.
    fn record_check(
        &self,
        start: Duration,
        end: Option<Duration>,
        result: Result<(), StopReason>,
        at: &'static Location<'static>,
    ) {
        let mut state = self.state.lock();
        if state.closed {
            // The span's statistics are final, but the stop was still observed.
            drop(state);
            self.observed(result, end);
            return;
        }
        let start = nanos(start);
        // A worker sharing this span may have recorded a later reading
        // first; an out-of-order arrival is a zero gap, not a regression.
        let gap = start.saturating_sub(state.last_check);
        if gap > state.max_check_gap {
            state.max_check_gap = gap;
            state.max_check_gap_start = state.last_check;
        }
        state.window.check(start);
        state.last_check = state.last_check.max(start);
        if let Some(end) = end {
            let elapsed = match nanos(end).checked_sub(start) {
                Some(elapsed) => elapsed,
                None => {
                    state.stats.clock_regressions += 1;
                    0
                }
            };
            state.check_time = state.check_time.saturating_add(elapsed);
            state.timed_checks += 1;
        }
        state.stats.overflowed |= add(&mut state.stats.checks, 1);
        if let (Err(reason), Some(end)) = (result, end)
            && state.stats.stopped_at.is_none()
        {
            state.stats.stopped_at = Some(end);
            state.stats.stop_reason = Some(reason);
        }
        let state = &mut *state;
        if let Some(i) = site(
            &mut state.stats,
            &mut state.site_gaps,
            &mut state.check_site,
            at,
        ) {
            let site = &mut state.stats.sites[i];
            site.checks = site.checks.saturating_add(1);
            state.site_gaps[i] = state.site_gaps[i].max(gap);
        }
        drop(state);
        self.observed(result, end);
    }

    /// Record when an instrumented check first returned a stop.
    fn observed(&self, result: Result<(), StopReason>, end: Option<Duration>) {
        if let (Err(_), Some(end)) = (result, end) {
            let mut trace = self.profiler.inner.state.lock();
            trace.observed_at = Some(trace.observed_at.unwrap_or(end).min(end));
        }
    }

    /// Record a report of `completed` units. Compiled once, like
    /// [`record_check`](Self::record_check).
    fn record_report(&self, completed: u64, at: &'static Location<'static>) {
        let now = if self.time_reports {
            Some(nanos(self.now()))
        } else {
            None
        };
        let mut state = self.state.lock();
        if state.closed {
            return;
        }
        if let Some(now) = now
            && now >= state.last_report
        {
            let at = Some(SourceSite::from_location(at));
            let duration = now - state.last_report;
            state.close_report_gap(now, duration, at);
            state.last_report = now;
            state.last_report_site = at;
        }
        state.stats.overflowed |= add(&mut state.stats.reports, 1);
        state.stats.overflowed |= add(&mut state.stats.units, completed);
        let state = &mut *state;
        if let Some(i) = site(
            &mut state.stats,
            &mut state.site_gaps,
            &mut state.report_site,
            at,
        ) {
            let site = &mut state.stats.sites[i];
            site.reports = site.reports.saturating_add(1);
            site.units = site.units.saturating_add(completed);
        }
    }

    fn finish(&self, outcome: Outcome) {
        let end = self.profiler.inner.clock.now();
        let stats = {
            let mut state = self.state.lock();
            if state.closed {
                return;
            }
            state.closed = true;
            let end = nanos(end);
            let gap = end.checked_sub(state.last_check).unwrap_or_else(|| {
                state.stats.clock_regressions += 1;
                0
            });
            if gap > state.max_check_gap {
                state.max_check_gap = gap;
                state.max_check_gap_start = state.last_check;
            }
            if self.time_reports {
                let duration = end.checked_sub(state.last_report).unwrap_or_else(|| {
                    state.stats.clock_regressions += 1;
                    0
                });
                state.close_report_gap(end, duration, None);
            }
            state.take_stats()
        };
        let record = SpanRecord {
            id: self.id,
            parent: self.parent,
            node: self.node,
            task: self.task.clone(),
            kind: self.kind,
            start: self.start,
            end,
            outcome,
            stats,
        };
        let mut trace = self.profiler.inner.state.lock();
        if self.id.0 != usize::MAX && trace.spans.len() < self.profiler.inner.capacity {
            trace.spans.push(record);
        } else {
            trace.dropped_spans = trace.dropped_spans.saturating_add(1);
        }
        self.profiler.inner.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The owner of one running span. Finish it after its instrumented calls
/// return; dropping it unfinished, including during unwinding, records
/// `Abandoned`.
#[must_use = "finish the span after its instrumented calls return; dropping it unfinished records Abandoned"]
pub struct Span {
    inner: Arc<SpanInner>,
    finished: bool,
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Span")
            .field("id", &self.inner.id)
            .field("task", &self.inner.task)
            .finish_non_exhaustive()
    }
}

impl Span {
    /// This span's identity.
    pub fn id(&self) -> SpanId {
        self.inner.id
    }
    /// Start a nested span, such as a callback, queue wait, or host suspension.
    pub fn child(&self, task: impl Into<String>, kind: SpanKind) -> Span {
        self.inner
            .profiler
            .start_span(Some(self.id()), self.inner.node, task.into(), kind, None)
    }
    /// Wrap a stop policy or progress sink so that its calls are recorded in
    /// this span, at their original call sites.
    pub fn instrument<T>(&self, value: T) -> Instrumented<T> {
        Instrumented {
            value,
            span: self.shared(),
        }
    }
    /// The recording state, for wrappers that record through
    /// [`checked`] and [`advanced`].
    pub(crate) fn shared(&self) -> Arc<SpanInner> {
        Arc::clone(&self.inner)
    }
    /// Record how the span ended. Wrappers keep working afterwards but stop
    /// recording.
    pub fn finish(mut self, outcome: Outcome) {
        self.inner.finish(outcome);
        self.finished = true;
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        if !self.finished {
            self.inner.finish(Outcome::Abandoned);
        }
    }
}

/// A stop policy or progress sink whose calls are recorded in a span.
///
/// It is `'static` whenever the wrapped value is, so it can go wherever the
/// original went: into a codec context, a spawned thread, or an
/// `Arc<dyn Stop>`. `may_stop` and `may_report` always return `true`, so
/// callers that skip no-op policies still make the calls being measured.
#[derive(Clone)]
pub struct Instrumented<T> {
    value: T,
    span: Arc<SpanInner>,
}

impl<T: fmt::Debug> fmt::Debug for Instrumented<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Instrumented")
            .field("value", &self.value)
            .field("span", &self.span.id)
            .finish()
    }
}

impl<T> Instrumented<T> {
    /// The wrapped value. Calls made through it are not recorded.
    pub fn inner(&self) -> &T {
        &self.value
    }
}

/// The indices `0..len`, stably ordered by `compare`: equal elements keep
/// their index order.
///
/// Every sort in this crate's analysis code goes through this one bottom-up
/// merge sort. The standard library's sorts compile to thousands of lines per
/// element type and comparator, which this cold code does not need.
pub(crate) fn sorted_indices(
    len: usize,
    compare: &dyn Fn(usize, usize) -> core::cmp::Ordering,
) -> Vec<usize> {
    let mut order = Vec::with_capacity(len);
    for index in 0..len {
        order.push(index);
    }
    let mut merged = order.clone();
    let mut width = 1;
    while width < len {
        let mut start = 0;
        while start < len {
            let middle = (start + width).min(len);
            let end = (start + 2 * width).min(len);
            let (mut left, mut right) = (start, middle);
            for slot in &mut merged[start..end] {
                let take_left =
                    right == end || (left < middle && compare(order[left], order[right]).is_le());
                if take_left {
                    *slot = order[left];
                    left += 1;
                } else {
                    *slot = order[right];
                    right += 1;
                }
            }
            start = end;
        }
        (order, merged) = (merged, order);
        width *= 2;
    }
    order
}

fn add(value: &mut u64, n: u64) -> bool {
    let overflow = value.checked_add(n).is_none();
    *value = value.saturating_add(n);
    overflow
}

/// Whether `site` is the call site at `at`: cheap fields first, and one
/// file's sites share its path's address.
#[inline(always)]
fn is_site(site: &SiteStats, at: &'static Location<'static>) -> bool {
    site.line == at.line()
        && site.column == at.column()
        && (core::ptr::eq(site.file, at.file()) || site.file == at.file())
}

/// The index in `stats.sites` of `at`, trying `hint` first and leaving it
/// there; `None` past 64 sites.
#[inline(always)]
fn site(
    stats: &mut Stats,
    gaps: &mut Vec<u64>,
    hint: &mut usize,
    at: &'static Location<'static>,
) -> Option<usize> {
    if stats.sites.get(*hint).is_some_and(|s| is_site(s, at)) {
        return Some(*hint);
    }
    find_site(stats, gaps, hint, at)
}

#[cold]
#[inline(never)]
fn find_site(
    stats: &mut Stats,
    gaps: &mut Vec<u64>,
    hint: &mut usize,
    at: &'static Location<'static>,
) -> Option<usize> {
    let sites = &mut stats.sites;
    let index = match sites.iter().position(|s| is_site(s, at)) {
        Some(index) => index,
        None if sites.len() == 64 => {
            stats.unattributed_calls = stats.unattributed_calls.saturating_add(1);
            return None;
        }
        None => {
            sites.push(SiteStats {
                file: at.file(),
                line: at.line(),
                column: at.column(),
                checks: 0,
                reports: 0,
                units: 0,
                max_gap_before_check: Duration::ZERO,
            });
            gaps.push(0);
            sites.len() - 1
        }
    };
    *hint = index;
    Some(index)
}

/// `value.check()`, recorded in `span` at the caller's location. Every
/// instrumented type calls this one body, so instrumenting another type adds
/// a few lines, not a copy of the bookkeeping.
#[track_caller]
pub(crate) fn checked(span: &SpanInner, value: &dyn Stop) -> Result<(), StopReason> {
    let timed = span.time_next_check();
    let start = span.now();
    let result = value.check(); // User policy never runs under our locks.
    let end = (timed || result.is_err()).then(|| span.now());
    span.record_check(start, end, result, Location::caller());
    result
}

/// `value.advance(completed)`, recorded in `span` like [`checked`].
#[track_caller]
pub(crate) fn advanced(span: &SpanInner, value: &dyn Report, completed: u64) {
    value.advance(completed);
    span.record_report(completed, Location::caller());
}

impl<T: Stop> Stop for Instrumented<T> {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        checked(&self.span, &self.value)
    }
}

impl<T: Report> Report for Instrumented<T> {
    #[track_caller]
    fn advance(&self, completed: u64) {
        advanced(&self.span, &self.value, completed);
    }
}

/// A finished run's spans, optionally with its progress tree attached.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Trace {
    /// Bounded reporting-protocol incidents, independent from work outcomes.
    pub incidents: Vec<crate::diagnostics::Incident>,
    /// Incidents omitted because their separate retention budget was exhausted.
    pub dropped_incidents: u64,

    /// Version of the JSON format.
    pub schema_version: u32,
    /// The progress tree, attached with [`with_progress`](Self::with_progress).
    pub progress: Option<how_far_along::Snapshot>,
    /// Key/value pairs from [`Profiler::metadata`].
    pub metadata: Vec<(String, String)>,
    /// Retained finished spans, in the order they finished.
    pub spans: Vec<SpanRecord>,
    /// Finished spans beyond the profiler's capacity.
    pub dropped_spans: u64,
    /// Spans still running when the trace was taken.
    pub active_spans: usize,
    /// When cancellation was requested, if recorded.
    pub cancelled_at: Option<Duration>,
    /// When an instrumented check first returned a stop error.
    pub observed_at: Option<Duration>,
    /// When the operation returned, if recorded.
    pub returned_at: Option<Duration>,
}

/// How a chosen set of independent work spans overlapped in time.
///
/// It measures concurrency, not CPU utilization.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Overlap {
    /// First start to last end, gaps included.
    pub wall: Duration,
    /// Sum of the spans' own durations.
    pub task_time: Duration,
    /// `task_time / wall`: the average number of spans running.
    pub mean_active_tasks: f64,
    /// The most spans running at once.
    pub peak_active_tasks: usize,
    /// The final stretch during which exactly one span was running.
    pub single_task_tail: Duration,
}

impl Trace {
    /// Attach the progress tree, usually a final snapshot taken after every
    /// worker joined.
    pub fn with_progress(mut self, progress: how_far_along::Snapshot) -> Self {
        self.progress = Some(progress);
        self
    }

    /// Measure how the given work spans overlapped.
    ///
    /// Returns `None` for an empty, missing or repeated ID, a span that is not
    /// `Work`, a span with clock regressions, or a span together with one of
    /// its ancestors (which would count time twice).
    pub fn overlap(&self, ids: &[SpanId]) -> Option<Overlap> {
        if ids.is_empty() {
            return None;
        }
        let (mut first, mut last, mut task_time) = (Duration::MAX, Duration::ZERO, Duration::ZERO);
        let mut events: Vec<(Duration, isize)> = Vec::with_capacity(2 * ids.len());
        for (i, &id) in ids.iter().enumerate() {
            let span = self.span(id)?;
            if ids[..i].contains(&id)
                || span.kind != SpanKind::Work
                || span.end < span.start
                || span.stats.clock_regressions != 0
            {
                return None;
            }
            let mut parent = span.parent;
            while let Some(id) = parent {
                if ids.contains(&id) {
                    return None;
                }
                parent = self.span(id)?.parent;
            }
            first = first.min(span.start);
            last = last.max(span.end);
            task_time = task_time.saturating_add(span.elapsed());
            events.push((span.start, 1));
            events.push((span.end, -1));
        }
        let order = sorted_indices(events.len(), &|a, b| events[a].0.cmp(&events[b].0));
        let mut active = 0_isize;
        let mut peak = 0;
        let mut tail_start = None;
        let mut i = 0;
        while i < order.len() {
            let at = events[order[i]].0;
            let before = active;
            while i < order.len() && events[order[i]].0 == at {
                active += events[order[i]].1;
                i += 1;
            }
            peak = peak.max(active as usize);
            if active == 1 && before != 1 {
                tail_start = Some(at);
            } else if active != 1 && at != last {
                tail_start = None;
            }
        }
        let wall = last.saturating_sub(first);
        Some(Overlap {
            wall,
            task_time,
            mean_active_tasks: if wall.is_zero() {
                0.0
            } else {
                task_time.as_secs_f64() / wall.as_secs_f64()
            },
            peak_active_tasks: peak,
            single_task_tail: last.saturating_sub(tail_start.unwrap_or(last)),
        })
    }

    /// The retained span with this identity.
    #[expect(
        clippy::manual_find,
        reason = "a loop compiles to less code than an adapter"
    )]
    pub(crate) fn span(&self, id: SpanId) -> Option<&SpanRecord> {
        for span in &self.spans {
            if span.id == id {
                return Some(span);
            }
        }
        None
    }

    /// Time from the cancellation request to the first check that saw it.
    pub fn cancellation_observation_latency(&self) -> Option<Duration> {
        self.observed_at?.checked_sub(self.cancelled_at?)
    }

    /// Time from the cancellation request to the operation's return,
    /// including stragglers, joins, and cleanup.
    pub fn cancellation_return_latency(&self) -> Option<Duration> {
        self.returned_at?.checked_sub(self.cancelled_at?)
    }

    /// Write the trace as JSON.
    ///
    /// Times are nanoseconds since the clock's epoch, written as decimal
    /// strings so 64-bit values survive JavaScript. Labels are escaped. Later
    /// versions may add keys; readers should ignore keys they do not know.
    pub fn write_json(&self, out: &mut impl fmt::Write) -> fmt::Result {
        write!(
            out,
            "{{\"schema_version\":{},\"time_unit\":\"ns\",\"dropped_spans\":{},\"active_spans\":{},\"metadata\":{{",
            self.schema_version, self.dropped_spans, self.active_spans
        )?;
        for (i, (key, value)) in self.metadata.iter().enumerate() {
            out.write_str(if i > 0 { "," } else { "" })?;
            quote(out, key)?;
            out.write_char(':')?;
            quote(out, value)?;
        }
        out.write_str("},\"progress\":")?;
        match &self.progress {
            Some(progress) => progress.write_json(out)?,
            None => out.write_str("null")?,
        }
        write!(
            out,
            ",\"dropped_incidents\":{},\"incidents\":[",
            self.dropped_incidents
        )?;
        for (i, incident) in self.incidents.iter().enumerate() {
            out.write_str(if i > 0 { "," } else { "" })?;
            write!(out, "{{\"node\":{},\"problem\":", incident.node)?;
            // Stable wire names; Debug text remains human-readable evidence only.
            let name = match incident.problem {
                crate::diagnostics::Problem::Plan(_) => "Plan",
                crate::diagnostics::Problem::ReportAfterCompletion => "ReportAfterCompletion",
                crate::diagnostics::Problem::ReportToBranch => "ReportToBranch",
            };
            quote(out, name)?;
            out.write_str(",\"detail\":")?;
            quote(out, &alloc::format!("{:?}", incident.problem))?;
            out.write_str(",\"file\":")?;
            quote(out, incident.site.file)?;
            write!(
                out,
                ",\"line\":{},\"column\":{}}}",
                incident.site.line, incident.site.column
            )?;
        }
        out.write_char(']')?;
        optional_time(out, ",\"cancelled_at\":", self.cancelled_at)?;
        optional_time(out, ",\"observed_at\":", self.observed_at)?;
        optional_time(out, ",\"returned_at\":", self.returned_at)?;
        out.write_str(",\"spans\":[")?;
        for (i, span) in self.spans.iter().enumerate() {
            out.write_str(if i > 0 { "," } else { "" })?;
            write!(out, "{{\"id\":{},\"parent\":", span.id)?;
            optional_number(out, span.parent.map(SpanId::get))?;
            out.write_str(",\"node\":")?;
            optional_number(out, span.node.map(NodeId::get))?;
            out.write_str(",\"task\":")?;
            quote(out, &span.task)?;
            write!(
                out,
                ",\"kind\":\"{}\",\"outcome\":\"{}\",\"start\":\"{}\",\"end\":\"{}\",\"checks\":{},\"reports\":{},\"units\":\"{}\",\"overflowed\":{},\"max_check_gap\":\"{}\",\"max_check_gap_start\":\"{}\",\"check_time\":\"{}\",\"clock_regressions\":{},\"unattributed_calls\":{}",
                span.kind.name(),
                outcome_name(span.outcome),
                span.start.as_nanos(),
                span.end.as_nanos(),
                span.stats.checks,
                span.stats.reports,
                span.stats.units,
                span.stats.overflowed,
                span.stats.max_check_gap.as_nanos(),
                span.stats.max_check_gap_start.as_nanos(),
                span.stats.check_time.as_nanos(),
                span.stats.clock_regressions,
                span.stats.unattributed_calls
            )?;
            optional_time(out, ",\"stopped_at\":", span.stats.stopped_at)?;
            out.write_str(",\"stop_reason\":")?;
            match span.stats.stop_reason {
                Some(reason) => write!(out, "\"{}\"", stop_reason_name(reason))?,
                None => out.write_str("null")?,
            }
            out.write_str(",\"max_report_gap\":")?;
            match &span.stats.max_report_gap {
                Some(gap) => {
                    write!(
                        out,
                        "{{\"start\":\"{}\",\"duration\":\"{}\",\"checks\":{},\"max_check_gap\":\"{}\",\"from\":",
                        gap.start.as_nanos(),
                        gap.duration.as_nanos(),
                        gap.checks,
                        gap.max_check_gap.as_nanos()
                    )?;
                    optional_site(out, gap.from)?;
                    out.write_str(",\"to\":")?;
                    optional_site(out, gap.to)?;
                    out.write_char('}')?;
                }
                None => out.write_str("null")?,
            }
            out.write_str(",\"sites\":[")?;
            for (j, site) in span.stats.sites.iter().enumerate() {
                out.write_str(if j > 0 { "," } else { "" })?;
                out.write_str("{\"file\":")?;
                quote(out, site.file)?;
                write!(
                    out,
                    ",\"line\":{},\"column\":{},\"checks\":{},\"reports\":{},\"units\":\"{}\",\"max_gap_before_check\":\"{}\"}}",
                    site.line,
                    site.column,
                    site.checks,
                    site.reports,
                    site.units,
                    site.max_gap_before_check.as_nanos()
                )?;
            }
            out.write_str("]}")?;
        }
        out.write_str("]}")
    }
}

impl fmt::Display for Trace {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            out,
            "how-far trace v{}: {} retained, {} active, {} dropped",
            self.schema_version,
            self.spans.len(),
            self.active_spans,
            self.dropped_spans
        )?;
        writeln!(
            out,
            "id\ttask\tkind\twall ms\tchecks\treports\tunits\tmax check gap ms\toutcome"
        )?;
        for span in &self.spans {
            writeln!(
                out,
                "{}\t{:?}\t{}\t{:.3}\t{}\t{}\t{}\t{:.3}\t{}",
                span.id,
                span.task,
                span.kind.name(),
                span.elapsed().as_secs_f64() * 1000.0,
                span.stats.checks,
                span.stats.reports,
                span.stats.units,
                span.stats.max_check_gap.as_secs_f64() * 1000.0,
                outcome_name(span.outcome)
            )?;
        }
        writeln!(out, "Times are wall-clock elapsed time, not CPU time.")
    }
}

fn optional_time(out: &mut impl fmt::Write, key: &str, value: Option<Duration>) -> fmt::Result {
    out.write_str(key)?;
    match value {
        Some(time) => write!(out, "\"{}\"", time.as_nanos()),
        None => out.write_str("null"),
    }
}

fn optional_number(out: &mut impl fmt::Write, value: Option<usize>) -> fmt::Result {
    match value {
        Some(number) => write!(out, "{number}"),
        None => out.write_str("null"),
    }
}

fn optional_site(out: &mut impl fmt::Write, value: Option<SourceSite>) -> fmt::Result {
    match value {
        Some(site) => {
            out.write_str("{\"file\":")?;
            quote(out, site.file)?;
            write!(out, ",\"line\":{},\"column\":{}}}", site.line, site.column)
        }
        None => out.write_str("null"),
    }
}

#[cfg(test)]
mod tests {
    use super::sorted_indices;
    use alloc::vec::Vec;

    #[test]
    fn exhausted_span_identifiers_are_dropped_not_a_panic() {
        use super::{Profiler, SpanKind, StdClock};
        use core::sync::atomic::Ordering;
        use how_far::Outcome;
        let profiler = Profiler::new(StdClock::new(), 8);
        profiler
            .inner
            .next_id
            .store(usize::MAX - 1, Ordering::Relaxed);
        for _ in 0..3 {
            profiler
                .span(None, "work", SpanKind::Work)
                .finish(Outcome::Succeeded);
        }
        let trace = profiler.snapshot();
        assert_eq!(trace.spans.len(), 1);
        assert_eq!(trace.dropped_spans, 2);
        assert_eq!(trace.active_spans, 0);
    }

    #[test]
    fn sorted_indices_is_a_stable_sort() {
        // A small LCG, so the test needs no dependencies and is reproducible.
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        for len in [0, 1, 2, 3, 4, 5, 7, 8, 9, 16, 31, 33, 100, 257] {
            for keys in [2, 7, 1_000] {
                let values: Vec<u64> = (0..len)
                    .map(|_| {
                        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                        (seed >> 33) % keys
                    })
                    .collect();
                let mut expected: Vec<usize> = (0..len).collect();
                expected.sort_by_key(|&i| values[i]); // The standard stable sort.
                assert_eq!(
                    sorted_indices(len, &|a, b| values[a].cmp(&values[b])),
                    expected,
                    "{len} values from {keys} keys"
                );
            }
        }
    }
}

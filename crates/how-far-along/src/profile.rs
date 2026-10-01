//! Opt-in evidence for tuning checkpoints and asymmetric parallel work.
//!
//! Create one [`Span`] per logical task/chunk (not one global meter for the
//! whole pool), then [`Span::instrument`] its stop/report value. Separate spans
//! describe queueing, joins, host yields, and callbacks. Nothing in this module
//! is enabled by ordinary reporting. Clocks and retention limits are explicit.
//! Recorded durations are elapsed wall time, **not CPU time**. This optional
//! collector requires `std`; it uses OS mutexes and belongs on native threads
//! or browser workers. Browser UI readers should use [`Profiler::try_snapshot`].

use crate::json::quote;
use crate::{Outcome, Report, Stop, StopReason, sync::Mutex};
use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::{
    fmt,
    panic::Location,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

/// A monotonic clock in a single shared epoch. Reads happen only in instrumented code.
/// Browser hosts supply a host clock; the collector still requires `std`.
pub trait Clock: Send + Sync {
    /// Elapsed time in the clock's epoch.
    fn now(&self) -> Duration;
}
impl<C: Clock + ?Sized> Clock for Arc<C> {
    fn now(&self) -> Duration {
        (**self).now()
    }
}

/// A monotonic host clock, with a fresh epoch at construction.
#[cfg(feature = "std")]
pub struct StdClock(std::time::Instant);
#[cfg(feature = "std")]
impl StdClock {
    /// Start a shared profiling epoch.
    pub fn new() -> Self {
        Self(std::time::Instant::now())
    }
}
#[cfg(feature = "std")]
impl Default for StdClock {
    fn default() -> Self {
        Self::new()
    }
}
#[cfg(feature = "std")]
impl Clock for StdClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

/// What a span measures. Work spans are distinct from waiting and callback overhead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SpanKind {
    /// Logical work execution (may still include nested waits/callbacks).
    Work,
    /// Work is queued and has not been claimed.
    Queued,
    /// Waiting, including a coordinator's join.
    Wait,
    /// A host suspension/yield interval.
    Yield,
    /// Arbitrary observer/application callback work.
    Callback,
}

/// Counts attributed to an actual library/application call site.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct SiteStats {
    /// Source file from `#[track_caller]`.
    pub file: &'static str,
    /// Source line.
    pub line: u32,
    /// Source column.
    pub column: u32,
    /// Cancellation checks, independent of reports.
    pub checks: u64,
    /// Reporting calls, independent of completed units.
    pub reports: u64,
    /// Completed units (saturated).
    pub units: u64,
    /// Longest gap ending at a check at this site, measured within this task.
    pub max_gap_before_check: Duration,
}

/// Per-task checkpoint evidence. Boundary gaps are included even with zero checks.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Stats {
    /// Number of checks (saturated).
    pub checks: u64,
    /// Number of reporting calls (saturated).
    pub reports: u64,
    /// Reported completed units (saturated).
    pub units: u64,
    /// Whether any count overflowed. Ratios then lose quantitative meaning.
    pub overflowed: bool,
    /// Entry-to-first-check, check-to-check, or last-check-to-exit maximum.
    pub max_check_gap: Duration,
    /// Time inside the wrapped checks, including inline callbacks.
    pub check_time: Duration,
    /// First observed Stop error in this task.
    pub stopped_at: Option<Duration>,
    /// Why the first stopped check failed, preserving cancellation versus timeout.
    pub stop_reason: Option<StopReason>,
    /// Clock regressions seen in this task; affected intervals are clamped to zero.
    pub clock_regressions: u64,
    /// At most 64 distinct call sites per span.
    pub sites: Vec<SiteStats>,
    /// Calls whose additional site identity exceeded the site budget. Aggregate counts remain complete.
    pub unattributed_calls: u64,
}

/// One completed execution span. IDs refer to this profiler run, not OS threads.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct SpanRecord {
    /// Stable run-local span ID.
    pub id: usize,
    /// Enclosing span ID, when explicitly supplied.
    pub parent: Option<usize>,
    /// Associated progress-tree node ID.
    pub node: usize,
    /// Logical task/chunk/attempt label, stable across worker scheduling choices.
    pub task: String,
    /// Execution, queueing, waiting, yield, or callback.
    pub kind: SpanKind,
    /// Start offset in the shared clock epoch.
    pub start: Duration,
    /// End offset in the same epoch.
    pub end: Duration,
    /// Explicit outcome; dropping a span records abandonment.
    pub outcome: Outcome,
    /// Check/report counts and site attribution.
    pub stats: Stats,
}
impl SpanRecord {
    /// Inclusive elapsed time. Do not sum parents and their nested spans.
    pub fn elapsed(&self) -> Duration {
        self.end.saturating_sub(self.start)
    }
    /// Check density, normalized by this task's elapsed seconds.
    pub fn checks_per_second(&self) -> f64 {
        if self.elapsed().is_zero() {
            0.0
        } else {
            self.stats.checks as f64 / self.elapsed().as_secs_f64()
        }
    }
    /// A consumer-selected storm heuristic, not a claim that frequent checks are wrong.
    /// Requires both sufficient evidence and a high per-task rate.
    pub fn is_poll_storm(&self, minimum_checks: u64, checks_per_second: f64) -> bool {
        !self.stats.overflowed
            && self.stats.checks >= minimum_checks
            && self.checks_per_second() > checks_per_second
    }
}

struct TraceState {
    metadata: Vec<(String, String)>,
    spans: Vec<SpanRecord>,
    dropped: u64,
    cancelled_at: Option<Duration>,
    observed_at: Option<Duration>,
    returned_at: Option<Duration>,
}
struct Inner {
    clock: Arc<dyn Clock>,
    capacity: usize,
    next_id: AtomicUsize,
    active: AtomicUsize,
    state: Mutex<TraceState>,
}

/// A bounded, clonable collector. No sampling threads, global state, or implicit clocks.
#[derive(Clone)]
pub struct Profiler {
    inner: Arc<Inner>,
}
impl Profiler {
    /// Retain up to `capacity` finished spans. Each live span retains at most 64 sites.
    /// Dropped spans are counted; incomplete coverage is visible in every export.
    pub fn new(clock: impl Clock + 'static, capacity: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                clock: Arc::new(clock),
                capacity,
                next_id: AtomicUsize::new(0),
                active: AtomicUsize::new(0),
                state: Mutex::new(TraceState {
                    metadata: Vec::new(),
                    spans: Vec::new(),
                    dropped: 0,
                    cancelled_at: None,
                    observed_at: None,
                    returned_at: None,
                }),
            }),
        }
    }
    /// Attach effective configuration, build/hardware identity, run/attempt IDs,
    /// predictor version, or a carefully scoped memory measurement. Replaces matching keys.
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
    /// Begin an independent task span. Use one for each asymmetric task or sampled chunk.
    pub fn span(&self, node: usize, task: impl Into<String>, kind: SpanKind) -> Span {
        self.start_span(None, node, task.into(), kind)
    }
    #[allow(deprecated)] // Atomic::try_update is newer than the Rust 1.88 MSRV.
    fn start_span(&self, parent: Option<usize>, node: usize, task: String, kind: SpanKind) -> Span {
        let id = self
            .inner
            .next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .expect("span identifiers exhausted");
        let start = self.inner.clock.now();
        self.inner.active.fetch_add(1, Ordering::Relaxed);
        Span {
            finished: false,
            inner: Arc::new(SpanInner {
                profiler: self.clone(),
                id,
                parent,
                node,
                task,
                kind,
                start,
                state: Mutex::new(SpanState {
                    stats: Stats::default(),
                    last_check: start,
                    closed: false,
                }),
            }),
        }
    }
    /// Record the actual cancellation request independently of the next check.
    /// Call beside the application's cancel operation, not when a subscriber eventually notices it.
    pub fn cancellation_requested(&self) {
        let now = self.inner.clock.now();
        let mut state = self.inner.state.lock();
        state.cancelled_at = Some(state.cancelled_at.map_or(now, |old| old.min(now)));
    }
    /// Record operation return after the last worker joined and cleanup completed.
    pub fn operation_returned(&self) {
        let now = self.inner.clock.now();
        self.inner.state.lock().returned_at = Some(now);
    }
    /// Clone finished records. Active and dropped span counts expose incomplete coverage.
    pub fn snapshot(&self) -> Trace {
        let state = self.inner.state.lock();
        self.snapshot_from(&state)
    }
    /// Nonblocking observation for a browser UI or other latency-sensitive owner.
    /// Return None on contention; retain the previous display and sample later.
    pub fn try_snapshot(&self) -> Option<Trace> {
        let state = self.inner.state.try_lock()?;
        Some(self.snapshot_from(&state))
    }
    fn snapshot_from(&self, state: &TraceState) -> Trace {
        Trace {
            schema_version: 1,
            progress: None,
            metadata: state.metadata.clone(),
            spans: state.spans.clone(),
            dropped_spans: state.dropped,
            active_spans: self.inner.active.load(Ordering::Relaxed),
            cancelled_at: state.cancelled_at,
            observed_at: state.observed_at,
            returned_at: state.returned_at,
        }
    }
}

struct SpanState {
    stats: Stats,
    last_check: Duration,
    closed: bool,
}
struct SpanInner {
    profiler: Profiler,
    id: usize,
    parent: Option<usize>,
    node: usize,
    task: String,
    kind: SpanKind,
    start: Duration,
    state: Mutex<SpanState>,
}
impl SpanInner {
    fn finish(&self, outcome: Outcome) {
        let end = self.profiler.inner.clock.now();
        let stats = {
            let mut state = self.state.lock();
            if state.closed {
                return;
            }
            state.closed = true;
            let gap = end.checked_sub(state.last_check).unwrap_or_else(|| {
                state.stats.clock_regressions += 1;
                Duration::ZERO
            });
            state.stats.max_check_gap = state.stats.max_check_gap.max(gap);
            core::mem::take(&mut state.stats)
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
        if trace.spans.len() < self.profiler.inner.capacity {
            trace.spans.push(record);
        } else {
            trace.dropped = trace.dropped.saturating_add(1);
        }
        self.profiler.inner.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Unique execution-span owner. Finish after its instrumented calls have returned.
/// Dropping a span records abandonment, including during unwinding.
pub struct Span {
    inner: Arc<SpanInner>,
    finished: bool,
}
impl Span {
    /// Span ID within this run.
    pub fn id(&self) -> usize {
        self.inner.id
    }
    /// Begin a nested interval, such as a callback, queue wait, or host yield.
    pub fn child(&self, task: impl Into<String>, kind: SpanKind) -> Span {
        self.inner
            .profiler
            .start_span(Some(self.id()), self.inner.node, task.into(), kind)
    }
    /// Attach independent check/report instrumentation to this task's value.
    /// A shared aggregate meter cannot reveal each worker's unpolled tail;
    /// create separate spans and wrappers for separate tasks.
    pub fn instrument<T>(&self, value: T) -> Instrumented<T> {
        Instrumented {
            value,
            span: Arc::clone(&self.inner),
        }
    }
    /// Publish an explicit outcome. The wrapper remains usable afterwards but
    /// no longer contributes measurements to this finished span.
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

/// A profiling adapter. `may_stop`/`may_report` stay true to preserve instrumentation
/// through erased/no-op seams. Caller locations pass through to the real call site.
#[derive(Clone)]
pub struct Instrumented<T> {
    value: T,
    span: Arc<SpanInner>,
}
impl<T> Instrumented<T> {
    /// Access the wrapped policy/sink without adding an instrumented event.
    pub fn inner(&self) -> &T {
        &self.value
    }
}
fn add(value: &mut u64, n: u64) -> bool {
    let overflow = value.checked_add(n).is_none();
    *value = value.saturating_add(n);
    overflow
}
fn site<'a>(stats: &'a mut Stats, at: &'static Location<'static>) -> Option<&'a mut SiteStats> {
    let position = stats
        .sites
        .iter()
        .position(|s| s.file == at.file() && s.line == at.line() && s.column == at.column());
    let index = if let Some(index) = position {
        index
    } else {
        if stats.sites.len() == 64 {
            stats.unattributed_calls = stats.unattributed_calls.saturating_add(1);
            return None;
        }
        stats.sites.push(SiteStats {
            file: at.file(),
            line: at.line(),
            column: at.column(),
            checks: 0,
            reports: 0,
            units: 0,
            max_gap_before_check: Duration::ZERO,
        });
        stats.sites.len() - 1
    };
    Some(&mut stats.sites[index])
}
impl<T: Stop> Stop for Instrumented<T> {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        let start = self.span.profiler.inner.clock.now();
        let result = self.value.check(); // Never run user policy under our locks.
        let end = self.span.profiler.inner.clock.now();
        let mut state = self.span.state.lock();
        if !state.closed {
            let gap = start.checked_sub(state.last_check).unwrap_or_else(|| {
                state.stats.clock_regressions += 1;
                Duration::ZERO
            });
            state.last_check = start;
            let elapsed = end.checked_sub(start).unwrap_or_else(|| {
                state.stats.clock_regressions += 1;
                Duration::ZERO
            });
            state.stats.check_time = state.stats.check_time.saturating_add(elapsed);
            state.stats.max_check_gap = state.stats.max_check_gap.max(gap);
            state.stats.overflowed |= add(&mut state.stats.checks, 1);
            if result.is_err() && state.stats.stopped_at.is_none() {
                state.stats.stopped_at = Some(end);
                state.stats.stop_reason = result.err();
            }
            if let Some(site) = site(&mut state.stats, Location::caller()) {
                site.checks = site.checks.saturating_add(1);
                site.max_gap_before_check = site.max_gap_before_check.max(gap);
            }
            drop(state);
            if result.is_err() {
                let mut trace = self.span.profiler.inner.state.lock();
                trace.observed_at = Some(trace.observed_at.map_or(end, |old| old.min(end)));
            }
        }
        result
    }
}
impl<T: Report> Report for Instrumented<T> {
    #[track_caller]
    fn advance(&self, completed: u64) {
        self.value.advance(completed);
        let mut state = self.span.state.lock();
        if state.closed {
            return;
        }
        state.stats.overflowed |= add(&mut state.stats.reports, 1);
        state.stats.overflowed |= add(&mut state.stats.units, completed);
        if let Some(site) = site(&mut state.stats, Location::caller()) {
            site.reports = site.reports.saturating_add(1);
            site.units = site.units.saturating_add(completed);
        }
    }
}

/// A versioned, owned run record; serializable without a framework dependency.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Trace {
    /// Version of the JSON schema.
    pub schema_version: u32,
    /// Optional accounting tree, with units, execution model, and estimate revisions.
    pub progress: Option<crate::Snapshot>,
    /// Caller-supplied effective configuration and run identity.
    pub metadata: Vec<(String, String)>,
    /// Retained completed spans, in finish order.
    pub spans: Vec<SpanRecord>,
    /// Completed spans excluded by the retention budget.
    pub dropped_spans: u64,
    /// Spans still open at sampling time.
    pub active_spans: usize,
    /// First actual cancellation request, if supplied by the caller.
    pub cancelled_at: Option<Duration>,
    /// First instrumented check returning a Stop error.
    pub observed_at: Option<Duration>,
    /// Operation return after joins, if supplied by the caller.
    pub returned_at: Option<Duration>,
}

/// Measured overlap of selected *independent* work spans, not inferred CPU utilization.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Overlap {
    /// First start to last finish (including any gaps).
    pub wall: Duration,
    /// Sum of selected execution intervals, in task-time units.
    pub task_time: Duration,
    /// Task-time divided by wall-time.
    pub mean_active_tasks: f64,
    /// Highest measured overlap.
    pub peak_active_tasks: usize,
    /// Final continuous interval with exactly one running selected task.
    pub single_task_tail: Duration,
}
impl Trace {
    /// Attach an accounting snapshot to make the execution trace self-describing.
    /// Use a terminal snapshot after joins when producing a final job artifact.
    pub fn with_progress(mut self, progress: crate::Snapshot) -> Self {
        self.progress = Some(progress);
        self
    }

    /// Summarize explicit work-span IDs. Returns None for missing/duplicate IDs,
    /// non-work spans, clock regressions, or ancestor/descendant pairs (which would
    /// double count). Selection lets callers compare each intermediate parallel group.
    pub fn overlap(&self, ids: &[usize]) -> Option<Overlap> {
        if ids.is_empty() {
            return None;
        }
        let mut spans = Vec::new();
        for (i, id) in ids.iter().enumerate() {
            if ids[..i].contains(id) {
                return None;
            }
            let span = self.spans.iter().find(|span| span.id == *id)?;
            if span.kind != SpanKind::Work
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
                parent = self.spans.iter().find(|s| s.id == id)?.parent;
            }
            spans.push(span);
        }
        let first = spans.iter().map(|s| s.start).min()?;
        let last = spans.iter().map(|s| s.end).max()?;
        let mut events: Vec<_> = spans
            .iter()
            .flat_map(|s| [(s.start, 1_isize), (s.end, -1)])
            .collect();
        events.sort_unstable_by_key(|e| e.0);
        let mut active = 0_isize;
        let mut peak = 0;
        let mut tail_start = None;
        let mut i = 0;
        while i < events.len() {
            let at = events[i].0;
            let before = active;
            while i < events.len() && events[i].0 == at {
                active += events[i].1;
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
        let task_time = spans
            .iter()
            .fold(Duration::ZERO, |sum, s| sum.saturating_add(s.elapsed()));
        Some(Overlap {
            wall,
            task_time,
            mean_active_tasks: if wall.is_zero() {
                0.0
            } else {
                task_time.as_secs_f64() / wall.as_secs_f64()
            },
            peak_active_tasks: peak,
            single_task_tail: tail_start.map_or(Duration::ZERO, |at| last.saturating_sub(at)),
        })
    }
    /// Request-to-first-observation latency. None if either endpoint is missing or regressed.
    pub fn cancellation_observation_latency(&self) -> Option<Duration> {
        self.observed_at?.checked_sub(self.cancelled_at?)
    }
    /// Request-to-return latency, including stragglers, joins, and cleanup.
    pub fn cancellation_return_latency(&self) -> Option<Duration> {
        self.returned_at?.checked_sub(self.cancelled_at?)
    }
    /// Export valid JSON with nanosecond offsets encoded as decimal strings to
    /// preserve integer precision in JavaScript. Schema and coverage are explicit.
    /// Human labels are escaped, including control characters.
    pub fn write_json(&self, out: &mut impl fmt::Write) -> fmt::Result {
        write!(
            out,
            "{{\"schema_version\":{},\"time_unit\":\"ns\",\"dropped_spans\":{},\"active_spans\":{},\"metadata\":{{",
            self.schema_version, self.dropped_spans, self.active_spans
        )?;
        for (i, (key, value)) in self.metadata.iter().enumerate() {
            if i > 0 {
                out.write_char(',')?;
            }
            quote(out, key)?;
            out.write_char(':')?;
            quote(out, value)?;
        }
        out.write_str("},\"progress\":")?;
        match &self.progress {
            Some(progress) => progress.write_json(out)?,
            None => out.write_str("null")?,
        }
        out.write_str(",\"cancelled_at\":")?;
        optional_time(out, self.cancelled_at)?;
        out.write_str(",\"observed_at\":")?;
        optional_time(out, self.observed_at)?;
        out.write_str(",\"returned_at\":")?;
        optional_time(out, self.returned_at)?;
        out.write_str(",\"spans\":[")?;
        for (i, span) in self.spans.iter().enumerate() {
            if i > 0 {
                out.write_char(',')?;
            }
            write!(
                out,
                "{{\"id\":{},\"parent\":{},\"node\":{},\"task\":",
                span.id,
                span.parent.map_or_else(|| "null".into(), |n| n.to_string()),
                span.node
            )?;
            quote(out, &span.task)?;
            write!(
                out,
                ",\"kind\":\"{:?}\",\"outcome\":\"{:?}\",\"start\":\"{}\",\"end\":\"{}\",\"checks\":{},\"reports\":{},\"units\":\"{}\",\"overflowed\":{},\"max_check_gap\":\"{}\",\"check_time\":\"{}\",\"clock_regressions\":{},\"unattributed_calls\":{},\"stopped_at\":",
                span.kind,
                span.outcome,
                span.start.as_nanos(),
                span.end.as_nanos(),
                span.stats.checks,
                span.stats.reports,
                span.stats.units,
                span.stats.overflowed,
                span.stats.max_check_gap.as_nanos(),
                span.stats.check_time.as_nanos(),
                span.stats.clock_regressions,
                span.stats.unattributed_calls
            )?;
            optional_time(out, span.stats.stopped_at)?;
            out.write_str(",\"stop_reason\":")?;
            match span.stats.stop_reason {
                Some(reason) => write!(out, "\"{reason:?}\"")?,
                None => out.write_str("null")?,
            }
            out.write_str(",\"sites\":[")?;
            for (j, site) in span.stats.sites.iter().enumerate() {
                if j > 0 {
                    out.write_char(',')?;
                }
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
            "how_far trace v{}: {} retained, {} active, {} dropped",
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
                "{}\t{:?}\t{:?}\t{:.3}\t{}\t{}\t{}\t{:.3}\t{:?}",
                span.id,
                span.task,
                span.kind,
                span.elapsed().as_secs_f64() * 1000.0,
                span.stats.checks,
                span.stats.reports,
                span.stats.units,
                span.stats.max_check_gap.as_secs_f64() * 1000.0,
                span.outcome
            )?;
        }
        writeln!(
            out,
            "Durations are inclusive elapsed time, not CPU time. Use independent span IDs for overlap; causes require application evidence."
        )
    }
}
fn optional_time(out: &mut impl fmt::Write, value: Option<Duration>) -> fmt::Result {
    match value {
        Some(time) => write!(out, "\"{}\"", time.as_nanos()),
        None => out.write_str("null"),
    }
}

//! Poll-latency instrumentation for [`Stop`] implementations.
//!
//! [`PollMeter`] wraps any `Stop` and records the wall-clock interval between
//! successive `check()`/`should_stop()` calls. It answers two questions that
//! matter for cooperative cancellation:
//!
//! - **Are we cancelling too slowly?** A gap longer than
//!   [`DEFAULT_SLOW_GAP`] between polls means a `cancel()` would go unobserved
//!   for that long — a responsiveness bug.
//! - **Are we polling too often?** Millions of polls separated by less than
//!   [`DEFAULT_FAST_GAP`] mean the checks themselves cost real time — a
//!   performance bug.
//!
//! Poll gaps are attributed to the *call site* via `#[track_caller]` +
//! [`Location::caller`], so a report identifies which `stop.check()` line in
//! the caller's code resumed polling after a long gap, and which site owned
//! the gap interval.
//!
//! # Example
//!
//! ```rust
//! use almost_enough::{PollMeter, Stop, Unstoppable};
//!
//! let meter = PollMeter::new(Unstoppable);
//! for _ in 0..4 {
//!     meter.check().unwrap();
//! }
//! let report = meter.report();
//! assert_eq!(report.calls, 4);
//! assert!(report.problems().is_empty());
//! ```

use crate::Stop;
use enough::StopReason;
use std::collections::HashMap;
use std::fmt;
use std::panic::Location;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Gap above which a poll interval is flagged: a cancel request could wait
/// this long before the next `check()` observes it.
pub const DEFAULT_SLOW_GAP: Duration = Duration::from_millis(50);

/// Mean gap below which polling is considered wasteful when the call count is
/// also huge — see [`DEFAULT_FLOOD_CALLS`].
pub const DEFAULT_FAST_GAP: Duration = Duration::from_micros(500);

/// Call count at which predominantly sub-[`DEFAULT_FAST_GAP`] polling is
/// flagged: the checks themselves have become a measurable cost.
pub const DEFAULT_FLOOD_CALLS: u64 = 1_000_000;

/// Number of 1-ms histogram buckets. Index `i` covers gaps in `[i, i+1)` ms;
/// gaps of 100 ms or more land in the overflow counter.
pub const HISTOGRAM_BUCKETS: usize = 100;

/// A source location where `check()`/`should_stop()` was invoked.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Site {
    file: &'static str,
    line: u32,
    column: u32,
}

impl Site {
    fn from_location(loc: &'static Location<'static>) -> Self {
        Self {
            file: loc.file(),
            line: loc.line(),
            column: loc.column(),
        }
    }
}

impl fmt::Display for Site {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.column)
    }
}

/// Per-site poll statistics. A gap is attributed to the site whose check call
/// *ended* the gap; the interval's owner (the site the gap started after) is
/// tracked separately for the worst gap.
#[derive(Clone, Debug, Default)]
struct SiteStats {
    /// Polls that arrived at this site.
    calls: u64,
    /// Polls arriving < [`DEFAULT_FAST_GAP`] after the previous poll.
    fast_gaps: u64,
    /// Polls arriving >= [`DEFAULT_SLOW_GAP`] after the previous poll.
    slow_gaps: u64,
    /// Longest gap ending at this site.
    max_gap_ns: u64,
    /// Sum of gaps ending at this site (for mean).
    total_gap_ns: u128,
}

/// A call site as `Location::caller` returns it. Polls look sites up by the
/// address of this static, which is cheap; the report merges any two
/// statics that name the same `file:line:column`.
type Caller = &'static Location<'static>;

#[derive(Debug)]
struct MeterState {
    /// Time of the previous recorded poll (`None` until the first poll; the
    /// meter's construction-to-first-poll interval is not meaningful work).
    last: Option<Instant>,
    /// Time of the first recorded poll (span = first→last poll).
    first: Instant,
    /// Index in `sites` of the previous poll's call site.
    last_site: Option<usize>,
    /// Total polls recorded.
    calls: u64,
    /// Histogram of inter-poll gaps, 1 ms per bucket; index 0 = `[0,1)` ms.
    buckets: [u64; HISTOGRAM_BUCKETS],
    /// Gaps >= 100 ms (beyond the histogram).
    overflow: u64,
    /// Gaps < [`DEFAULT_FAST_GAP`].
    fast_gaps: u64,
    /// Gaps >= [`DEFAULT_SLOW_GAP`].
    slow_gaps: u64,
    /// Sum of all recorded gaps.
    total_gap_ns: u128,
    /// Longest single gap, and the indices in `sites` of the pair it ran
    /// between.
    max_gap_ns: u64,
    max_gap_from: Option<usize>,
    max_gap_to: Option<usize>,
    /// Per-call-site statistics, in order of first poll.
    sites: Vec<(Caller, SiteStats)>,
    /// Index in `sites` by the caller's address.
    index: HashMap<usize, usize>,
}

impl MeterState {
    fn new() -> Self {
        Self {
            last: None,
            first: Instant::now(),
            last_site: None,
            calls: 0,
            buckets: [0; HISTOGRAM_BUCKETS],
            overflow: 0,
            fast_gaps: 0,
            slow_gaps: 0,
            total_gap_ns: 0,
            max_gap_ns: 0,
            max_gap_from: None,
            max_gap_to: None,
            sites: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// The index of `caller` in `sites`: the previous poll's site without a
    /// lookup, since loops mostly poll from one place.
    fn site(&mut self, caller: Caller) -> usize {
        if let Some(last) = self.last_site {
            if core::ptr::eq(self.sites[last].0, caller) {
                return last;
            }
        }
        let sites = &mut self.sites;
        *self
            .index
            .entry(core::ptr::from_ref(caller).addr())
            .or_insert_with(|| {
                sites.push((caller, SiteStats::default()));
                sites.len() - 1
            })
    }

    fn record(&mut self, caller: Caller, now: Instant) {
        if self.last.is_none() {
            self.first = now;
        }
        self.calls += 1;
        let site = self.site(caller);
        let site_stats = &mut self.sites[site].1;
        site_stats.calls += 1;
        if let Some(last) = self.last {
            let gap_ns = now.saturating_duration_since(last).as_nanos();
            self.total_gap_ns += gap_ns;
            site_stats.total_gap_ns += gap_ns;
            let ns64 = u64::try_from(gap_ns).unwrap_or(u64::MAX);
            if ns64 > site_stats.max_gap_ns {
                site_stats.max_gap_ns = ns64;
            }
            let gap = Duration::from_nanos(ns64);
            if gap < DEFAULT_FAST_GAP {
                self.fast_gaps += 1;
                site_stats.fast_gaps += 1;
            }
            if gap >= DEFAULT_SLOW_GAP {
                self.slow_gaps += 1;
                site_stats.slow_gaps += 1;
            }
            let ms = usize::try_from(gap.as_millis()).unwrap_or(usize::MAX);
            if ms < HISTOGRAM_BUCKETS {
                self.buckets[ms] += 1;
            } else {
                self.overflow += 1;
            }
            if ns64 > self.max_gap_ns {
                self.max_gap_ns = ns64;
                self.max_gap_from = self.last_site;
                self.max_gap_to = Some(site);
            }
        }
        self.last = Some(now);
        self.last_site = Some(site);
    }
}

/// A [`Stop`] wrapper that measures time between polls.
///
/// `PollMeter` records every `check()`/`should_stop()` call — timestamp,
/// interval since the previous poll, and call site — into shared state, then
/// delegates to the wrapped stop. Cloning the meter shares the same recording
/// state, so a meter cloned into worker threads produces one unified report.
///
/// Each poll reads the clock once and updates the shared state under a
/// mutex: about 300 instructions on x86-64, half of them the clock read.
/// Call sites are looked up by the address of their `Location`, and a poll
/// from the same site as the previous one skips the lookup. Do not meter a
/// poll site that is itself called in a tight inner loop in production; the
/// meter is meant for tests, fuzzing, and perf investigations.
///
/// # Example
///
/// ```rust
/// use almost_enough::{PollMeter, Stop, Stopper};
///
/// let stopper = Stopper::new();
/// let meter = PollMeter::new(stopper.clone());
///
/// for _ in 0..10 {
///     meter.check().unwrap();
/// }
/// stopper.cancel();
/// assert!(meter.check().is_err());
///
/// let report = meter.report();
/// assert_eq!(report.calls, 11);
/// ```
pub struct PollMeter<S> {
    inner: S,
    state: Arc<Mutex<MeterState>>,
}

impl<S> PollMeter<S> {
    /// Wrap `inner` in a metered stop token.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            state: Arc::new(Mutex::new(MeterState::new())),
        }
    }

    /// Borrow the wrapped stop.
    pub fn inner(&self) -> &S {
        &self.inner
    }

    /// Unwrap and return the inner stop.
    pub fn into_inner(self) -> S {
        self.inner
    }

    /// Snapshot the recorded statistics.
    ///
    /// # Panics
    /// If the internal mutex was poisoned by a panic during a poll.
    pub fn report(&self) -> PollReport {
        let s = self.state.lock().expect("PollMeter mutex poisoned");
        // Merge statics that name the same location, keeping first-poll order.
        let mut merged: Vec<(Site, SiteStats)> = Vec::new();
        let mut by_site: HashMap<Site, usize> = HashMap::new();
        for (caller, st) in &s.sites {
            let site = Site::from_location(caller);
            let i = *by_site.entry(site).or_insert_with(|| {
                merged.push((site, SiteStats::default()));
                merged.len() - 1
            });
            let into = &mut merged[i].1;
            into.calls += st.calls;
            into.fast_gaps += st.fast_gaps;
            into.slow_gaps += st.slow_gaps;
            into.max_gap_ns = into.max_gap_ns.max(st.max_gap_ns);
            into.total_gap_ns += st.total_gap_ns;
        }
        let mut sites: Vec<SiteReport> = merged
            .iter()
            .map(|(site, st)| SiteReport {
                site: site.to_string(),
                calls: st.calls,
                fast_gaps: st.fast_gaps,
                slow_gaps: st.slow_gaps,
                max_gap: Duration::from_nanos(st.max_gap_ns),
                mean_gap: mean_duration(st.total_gap_ns, st.calls),
            })
            .collect();
        let name = |i: usize| Site::from_location(s.sites[i].0).to_string();
        // Slowest sites first.
        sites.sort_by_key(|r| core::cmp::Reverse(r.max_gap));
        let span = match (s.first, s.last) {
            (f, Some(l)) => l.saturating_duration_since(f),
            _ => Duration::ZERO,
        };
        PollReport {
            calls: s.calls,
            fast_gaps: s.fast_gaps,
            slow_gaps: s.slow_gaps,
            mean_gap: mean_duration(s.total_gap_ns, s.calls.saturating_sub(1)),
            max_gap: Duration::from_nanos(s.max_gap_ns),
            max_gap_from: s.max_gap_from.map(name),
            max_gap_to: s.max_gap_to.map(name),
            span,
            histogram_ms: s.buckets,
            overflow_ms: s.overflow,
            sites,
        }
    }

    /// Clear all recorded statistics and start measuring again.
    ///
    /// # Panics
    /// If the internal mutex was poisoned by a panic during a poll.
    pub fn reset(&self) {
        *self.state.lock().expect("PollMeter mutex poisoned") = MeterState::new();
    }
}

fn mean_duration(total_ns: u128, count: u64) -> Duration {
    if count == 0 {
        return Duration::ZERO;
    }
    let ns = u64::try_from(total_ns / count as u128).unwrap_or(u64::MAX);
    Duration::from_nanos(ns)
}

impl<S: Clone> Clone for PollMeter<S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            state: Arc::clone(&self.state),
        }
    }
}

impl<S> fmt::Debug for PollMeter<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PollMeter").finish_non_exhaustive()
    }
}

impl<S: Stop> Stop for PollMeter<S> {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.record(Location::caller());
        self.inner.check()
    }

    #[track_caller]
    fn should_stop(&self) -> bool {
        self.record(Location::caller());
        self.inner.should_stop()
    }

    /// Always returns `true`, even when the inner stop can never stop.
    ///
    /// Wrapping [`Unstoppable`] still records polls — that is the point of the
    /// meter. Reporting `false` here would let callers (e.g.
    /// `StopToken::new`'s no-op collapse) erase the instrumentation.
    ///
    /// [`Unstoppable`]: crate::Unstoppable
    #[inline]
    fn may_stop(&self) -> bool {
        true
    }
}

impl<S> PollMeter<S> {
    fn record(&self, caller: Caller) {
        self.state
            .lock()
            .expect("PollMeter mutex poisoned")
            .record(caller, Instant::now());
    }
}

/// Snapshot of poll-latency statistics from a [`PollMeter`].
#[derive(Clone, Debug)]
pub struct PollReport {
    /// Total `check()`/`should_stop()` calls recorded.
    pub calls: u64,
    /// Gaps shorter than [`DEFAULT_FAST_GAP`] (0.5 ms).
    pub fast_gaps: u64,
    /// Gaps at or above [`DEFAULT_SLOW_GAP`] (50 ms).
    pub slow_gaps: u64,
    /// Mean gap between polls.
    pub mean_gap: Duration,
    /// Longest single gap between polls.
    pub max_gap: Duration,
    /// Call site where the longest gap started (`file:line:column`).
    pub max_gap_from: Option<String>,
    /// Call site where the longest gap ended.
    pub max_gap_to: Option<String>,
    /// Wall-clock span from first to last poll.
    pub span: Duration,
    /// Inter-poll gap histogram; index `i` counts gaps in `[i, i+1)` ms.
    pub histogram_ms: [u64; HISTOGRAM_BUCKETS],
    /// Gaps of 100 ms or more.
    pub overflow_ms: u64,
    /// Per-call-site statistics, sorted by `max_gap` descending.
    pub sites: Vec<SiteReport>,
}

/// Statistics for one `check()`/`should_stop()` call site.
#[derive(Clone, Debug)]
pub struct SiteReport {
    /// `file:line:column` of the call site.
    pub site: String,
    /// Polls recorded at this site.
    pub calls: u64,
    /// Gaps ending at this site shorter than [`DEFAULT_FAST_GAP`].
    pub fast_gaps: u64,
    /// Gaps ending at this site at or above [`DEFAULT_SLOW_GAP`].
    pub slow_gaps: u64,
    /// Longest gap ending at this site.
    pub max_gap: Duration,
    /// Mean gap ending at this site.
    pub mean_gap: Duration,
}

/// A poll pattern worth flagging, returned by [`PollReport::problems`].
#[derive(Clone, Debug, PartialEq)]
pub enum PollProblem {
    /// A gap between polls met or exceeded the slow threshold — cancellation
    /// could wait this long before being observed.
    SlowGap {
        /// The worst observed gap.
        gap: Duration,
        /// Site where the gap ended (`file:line:column`), if known.
        site: Option<String>,
        /// How many gaps exceeded the threshold.
        count: u64,
    },
    /// Poll volume is enormous and gaps are mostly sub-threshold — the checks
    /// themselves have become a measurable cost.
    PollStorm {
        /// Total polls recorded.
        calls: u64,
        /// Mean gap between polls.
        mean_gap: Duration,
    },
}

impl PollReport {
    /// Evaluate the report against the default thresholds
    /// ([`DEFAULT_SLOW_GAP`], [`DEFAULT_FAST_GAP`], [`DEFAULT_FLOOD_CALLS`]).
    pub fn problems(&self) -> Vec<PollProblem> {
        self.problems_with(DEFAULT_SLOW_GAP, DEFAULT_FAST_GAP, DEFAULT_FLOOD_CALLS)
    }

    /// Evaluate the report against explicit thresholds.
    pub fn problems_with(
        &self,
        slow_gap: Duration,
        fast_gap: Duration,
        flood_calls: u64,
    ) -> Vec<PollProblem> {
        let mut out = Vec::new();
        if self.max_gap >= slow_gap {
            out.push(PollProblem::SlowGap {
                gap: self.max_gap,
                site: self.max_gap_to.clone(),
                count: self.slow_gaps,
            });
        }
        if self.calls >= flood_calls && self.mean_gap < fast_gap && self.fast_gaps * 2 >= self.calls
        {
            out.push(PollProblem::PollStorm {
                calls: self.calls,
                mean_gap: self.mean_gap,
            });
        }
        out
    }

    /// Render the ms-bucketed gap histogram as ASCII bars.
    ///
    /// Only non-zero buckets are printed; the 50 ms boundary and the
    /// [`DEFAULT_SLOW_GAP`] flag are marked so problem rows stand out.
    /// `width` is the maximum bar length in characters.
    pub fn histogram_ascii(&self, width: usize) -> String {
        let mut out = String::new();
        let max = self
            .histogram_ms
            .iter()
            .copied()
            .chain(core::iter::once(self.overflow_ms))
            .max()
            .unwrap_or(0);
        if max == 0 {
            return "  (no gaps recorded)\n".into();
        }
        let width = width.max(1);
        for (i, &count) in self.histogram_ms.iter().enumerate() {
            if count == 0 {
                continue;
            }
            let bar = ((count as u128 * width as u128) / max as u128).max(1) as usize;
            let flag = if Duration::from_millis(i as u64 + 1) > DEFAULT_SLOW_GAP {
                "  <-- slow"
            } else {
                ""
            };
            out.push_str(&format!(
                "{:>3}-{:>3}ms |{:<width$} {:>9}{flag}\n",
                i,
                i + 1,
                "#".repeat(bar),
                count,
                width = width,
            ));
        }
        if self.overflow_ms > 0 {
            let bar = ((self.overflow_ms as u128 * width as u128) / max as u128).max(1) as usize;
            out.push_str(&format!(
                " >=100ms |{:<width$} {:>9}  <-- slow\n",
                "#".repeat(bar),
                self.overflow_ms,
                width = width,
            ));
        }
        out
    }
}

impl fmt::Display for PollReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "poll report: {} calls over {:?} (mean gap {:?}, max gap {:?}",
            self.calls, self.span, self.mean_gap, self.max_gap
        )?;
        if let (Some(from), Some(to)) = (&self.max_gap_from, &self.max_gap_to) {
            write!(f, ", {from} -> {to}")?;
        }
        writeln!(f, ")")?;
        let problems = self.problems();
        if problems.is_empty() {
            writeln!(f, "  no poll-latency problems")?;
        } else {
            for p in &problems {
                match p {
                    PollProblem::SlowGap { gap, site, count } => writeln!(
                        f,
                        "  SLOW: {} gap(s) >= {}ms, worst {:?} ending at {}",
                        count,
                        DEFAULT_SLOW_GAP.as_millis(),
                        gap,
                        site.as_deref().unwrap_or("<unknown>"),
                    )?,
                    PollProblem::PollStorm { calls, mean_gap } => writeln!(
                        f,
                        "  STORM: {calls} polls, mean gap {mean_gap:?} (< {:?} threshold)",
                        DEFAULT_FAST_GAP,
                    )?,
                }
            }
        }
        if f.alternate() {
            write!(f, "{}", self.histogram_ascii(30))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StopSource, Unstoppable};

    #[test]
    fn records_calls() {
        let meter = PollMeter::new(Unstoppable);
        for _ in 0..10 {
            meter.check().unwrap();
        }
        let report = meter.report();
        assert_eq!(report.calls, 10);
        assert_eq!(report.slow_gaps, 0);
        assert!(report.problems().is_empty());
    }

    #[test]
    fn records_should_stop_too() {
        let meter = PollMeter::new(Unstoppable);
        for _ in 0..7 {
            assert!(!meter.should_stop());
        }
        assert_eq!(meter.report().calls, 7);
    }

    #[test]
    fn delegates_cancellation() {
        let stopper = crate::Stopper::new();
        let meter = PollMeter::new(stopper.clone());
        assert!(!meter.should_stop());
        stopper.cancel();
        assert!(meter.should_stop());
        assert_eq!(meter.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn attributes_call_site() {
        fn poll_here<S: Stop>(s: &S) {
            s.check().unwrap(); // this line is the expected site
        }
        let meter = PollMeter::new(Unstoppable);
        poll_here(&meter);
        poll_here(&meter);
        let report = meter.report();
        assert_eq!(report.calls, 2);
        assert_eq!(report.sites.len(), 1);
        assert!(report.sites[0].site.contains("meter.rs"));
    }

    #[test]
    fn separates_sites() {
        let meter = PollMeter::new(Unstoppable);
        meter.check().unwrap();
        meter.check().unwrap(); // different line -> different site
        let report = meter.report();
        assert_eq!(report.sites.len(), 2);
    }

    #[test]
    fn flags_slow_gap() {
        let meter = PollMeter::new(Unstoppable);
        meter.check().unwrap();
        std::thread::sleep(Duration::from_millis(60));
        meter.check().unwrap();
        let report = meter.report();
        assert_eq!(report.slow_gaps, 1);
        assert!(report.max_gap >= Duration::from_millis(50));
        assert!(matches!(
            report.problems().as_slice(),
            [PollProblem::SlowGap { .. }]
        ));
    }

    #[test]
    fn histogram_counts_ms_buckets() {
        let meter = PollMeter::new(Unstoppable);
        meter.check().unwrap(); // first call: no gap
        std::thread::sleep(Duration::from_millis(2));
        meter.check().unwrap();
        std::thread::sleep(Duration::from_millis(55));
        meter.check().unwrap();
        let report = meter.report();
        assert_eq!(report.histogram_ms[2], 1);
        assert_eq!(report.slow_gaps, 1);
        let ascii = report.histogram_ascii(20);
        assert!(ascii.contains(" 55- 56ms") || ascii.contains("55- 56"));
    }

    #[test]
    fn alternating_sites_keep_their_own_counts_and_gaps() {
        // Each switch misses the previous-site shortcut and looks the site up.
        let meter = PollMeter::new(Unstoppable);
        let (a, b) = (
            |m: &PollMeter<Unstoppable>| m.check(),
            |m: &PollMeter<Unstoppable>| m.check(),
        );
        for round in 0..10 {
            a(&meter).unwrap();
            if round == 4 {
                std::thread::sleep(Duration::from_millis(60));
            }
            b(&meter).unwrap();
            b(&meter).unwrap();
        }
        let report = meter.report();
        assert_eq!(report.calls, 30);
        assert_eq!(report.sites.len(), 2);
        // Sorted by max gap: the slow gap ended at `b`'s site.
        assert_eq!(report.sites[0].calls, 20);
        assert_eq!(report.sites[0].slow_gaps, 1);
        assert_eq!(report.sites[1].calls, 10);
        assert_eq!(report.sites[1].slow_gaps, 0);
        assert_eq!(
            report.max_gap_to.as_deref(),
            Some(report.sites[0].site.as_str())
        );
        assert_eq!(
            report.max_gap_from.as_deref(),
            Some(report.sites[1].site.as_str())
        );
        assert_ne!(report.sites[0].site, report.sites[1].site);
    }

    #[test]
    fn reset_clears() {
        let meter = PollMeter::new(Unstoppable);
        for _ in 0..5 {
            meter.check().unwrap();
        }
        meter.reset();
        let report = meter.report();
        assert_eq!(report.calls, 0);
    }

    #[test]
    fn clone_shares_state() {
        let meter = PollMeter::new(crate::Stopper::new());
        let m2 = meter.clone();
        meter.check().unwrap();
        m2.check().unwrap();
        assert_eq!(meter.report().calls, 2);
    }

    #[test]
    fn may_stop_always_true() {
        // The meter must never be optimized away — even over Unstoppable —
        // or instrumentation would see no polls.
        assert!(PollMeter::new(Unstoppable).may_stop());
        assert!(PollMeter::new(StopSource::new()).may_stop());
    }

    #[test]
    fn survives_stop_token_collapse() {
        // StopToken::new collapses may_stop()==false to a no-op; the meter
        // must stay live inside one.
        let meter = PollMeter::new(Unstoppable);
        let token = crate::StopToken::new(meter.clone());
        token.check().unwrap();
        assert_eq!(meter.report().calls, 1);
    }

    #[test]
    fn storm_detection() {
        // Simulate a flood by lying about counts — build a report directly.
        let mut report = PollReport {
            calls: 0,
            fast_gaps: 0,
            slow_gaps: 0,
            mean_gap: Duration::ZERO,
            max_gap: Duration::ZERO,
            max_gap_from: None,
            max_gap_to: None,
            span: Duration::ZERO,
            histogram_ms: [0; HISTOGRAM_BUCKETS],
            overflow_ms: 0,
            sites: vec![],
        };
        assert!(report.problems().is_empty());
        report.calls = 2_000_000;
        report.mean_gap = Duration::from_micros(100);
        report.fast_gaps = 1_900_000;
        assert!(matches!(
            report.problems().as_slice(),
            [PollProblem::PollStorm { .. }]
        ));
    }

    #[test]
    fn display_renders() {
        let meter = PollMeter::new(Unstoppable);
        for _ in 0..3 {
            meter.check().unwrap();
        }
        let s = format!("{:#}", meter.report());
        assert!(s.contains("poll report"));
        assert!(s.contains("no poll-latency problems"));
    }

    #[test]
    fn meter_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PollMeter<Unstoppable>>();
    }
}

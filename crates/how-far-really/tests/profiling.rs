use almost_enough::Stopper;
use how_far::{NoReport, ProgressWithStop};
use how_far_along::{NodeId, Outcome, ProgressExt, Report, Stop, StopReason, Unstoppable};
use how_far_really::profile::{Clock, Profiler, SpanKind};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Default)]
struct ManualClock(Arc<AtomicU64>);
impl ManualClock {
    fn set(&self, millis: u64) {
        self.0.store(millis, Ordering::Relaxed);
    }
}
impl Clock for ManualClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::Relaxed))
    }
}

#[test]
fn asymmetric_overlap_exposes_straggler_without_claiming_cpu_utilization() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 4);
    let spans: Vec<_> = (0..4)
        .map(|i| profiler.span(None, format!("task-{i}"), SpanKind::Work))
        .collect();
    let ids: Vec<_> = spans.iter().map(|span| span.id()).collect();
    for (span, end) in spans.into_iter().zip([12, 20, 22, 90]) {
        clock.set(end);
        span.finish(Outcome::Succeeded);
    }
    let trace = profiler.snapshot();
    let overlap = trace.overlap(&ids).unwrap();
    assert_eq!(overlap.wall, Duration::from_millis(90));
    assert_eq!(overlap.task_time, Duration::from_millis(144));
    assert_eq!(overlap.peak_active_tasks, 4);
    assert!((overlap.mean_active_tasks - 1.6).abs() < 1e-12);
    assert_eq!(overlap.single_task_tail, Duration::from_millis(68));
    assert_eq!(
        trace.spans[3].stats.max_check_gap,
        Duration::from_millis(90)
    );
    assert!(trace.overlap(&[ids[0], ids[0]]).is_none());
    assert!(trace.overlap(&[]).is_none());
    // A span beyond the profiler's capacity is not retained, so it is missing.
    let extra = profiler.span(None, "over capacity", SpanKind::Work);
    let missing = extra.id();
    extra.finish(Outcome::Succeeded);
    assert_eq!(profiler.snapshot().dropped_spans, 1);
    assert!(profiler.snapshot().overlap(&[missing]).is_none());
}

#[test]
fn per_task_boundary_gaps_cannot_be_masked_by_another_busy_worker() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 2);
    let busy = profiler.span(None, "frequent", SpanKind::Work);
    let silent = profiler.span(None, "unpolled tail", SpanKind::Work);
    let busy_stop = busy.instrument(Unstoppable);
    let silent_stop = silent.instrument(Unstoppable);
    clock.set(2);
    silent_stop.check().unwrap();
    for millis in 3..100 {
        clock.set(millis);
        busy_stop.check().unwrap();
    }
    clock.set(100);
    busy.finish(Outcome::Succeeded);
    silent.finish(Outcome::Succeeded);
    let trace = profiler.snapshot();
    assert_eq!(trace.spans[0].stats.max_check_gap, Duration::from_millis(3));
    assert_eq!(
        trace.spans[1].stats.max_check_gap,
        Duration::from_millis(98)
    );
}

#[test]
fn check_storms_counts_units_and_original_sites_stay_separate() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 1);
    let span = profiler.span(None, "candidate search", SpanKind::Work);
    let work = span.instrument(ProgressWithStop::new(Unstoppable, NoReport));
    let mut checks_line = 0;
    for _ in 0..10_000 {
        checks_line = line!() + 1;
        work.check().unwrap();
    }
    let reports_line = line!() + 1;
    work.advance(16);
    let step_line = line!() + 1;
    work.step(1).unwrap();
    clock.set(1);
    span.finish(Outcome::Succeeded);
    let trace = profiler.snapshot();
    let record = &trace.spans[0];
    assert_eq!(
        (
            record.stats.checks,
            record.stats.reports,
            record.stats.units
        ),
        (10_001, 2, 17)
    );
    assert!(record.is_poll_storm(1_000, 1_000_000.0));
    assert!(!record.is_poll_storm(1_000_000, 1_000_000.0));
    let site = |line| record.stats.sites.iter().find(|s| s.line == line).unwrap();
    assert_eq!(site(checks_line).checks, 10_000);
    assert_eq!(site(reports_line).units, 16);
    let step = site(step_line);
    assert_eq!((step.checks, step.reports), (1, 1));
    assert!(record.stats.sites.iter().all(|s| s.file == file!()));
}

#[test]
fn cancellation_measurements_include_observation_and_join_cleanup_tail() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 2);
    let span = profiler.span(None, "worker", SpanKind::Work);
    let stopper = Stopper::new();
    let stop = span.instrument(stopper.clone());
    clock.set(10);
    profiler.cancellation_requested();
    stopper.cancel();
    clock.set(17);
    assert_eq!(stop.check(), Err(StopReason::Cancelled));
    clock.set(24);
    span.finish(Outcome::Cancelled);
    let join = profiler.span(None, "join and cleanup", SpanKind::Wait);
    clock.set(35);
    join.finish(Outcome::Cancelled);
    profiler.operation_returned();
    let trace = profiler.snapshot();
    assert_eq!(
        trace.cancellation_observation_latency(),
        Some(Duration::from_millis(7))
    );
    assert_eq!(
        trace.cancellation_return_latency(),
        Some(Duration::from_millis(25))
    );
    assert_eq!(
        trace.spans[0].stats.stopped_at,
        Some(Duration::from_millis(17))
    );
}

#[test]
fn nested_callback_wait_and_yield_spans_do_not_double_count_execution() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 8);
    let outer = profiler.span(NodeId::ROOT, "outer", SpanKind::Work);
    let inner = outer.child("nested work", SpanKind::Work);
    let ids = [outer.id(), inner.id()];
    clock.set(1);
    inner.finish(Outcome::Succeeded);
    for kind in [
        SpanKind::Queued,
        SpanKind::Callback,
        SpanKind::Yield,
        SpanKind::Wait,
    ] {
        let nested = outer.child(format!("{kind:?}"), kind);
        clock.0.fetch_add(1, Ordering::Relaxed);
        nested.finish(Outcome::Succeeded);
    }
    outer.finish(Outcome::Succeeded);
    let trace = profiler.snapshot();
    assert!(trace.overlap(&ids).is_none());
    assert_eq!(
        trace
            .spans
            .iter()
            .filter(|s| s.parent == Some(ids[0]))
            .count(),
        5
    );
}

#[test]
fn arbitrary_stop_callback_time_is_measured_without_holding_collector_locks() {
    struct Callback {
        clock: ManualClock,
        profiler: Profiler,
    }
    impl Stop for Callback {
        fn check(&self) -> Result<(), StopReason> {
            let nested = self.profiler.span(None, "callback", SpanKind::Callback);
            self.profiler.metadata("reentered", "yes");
            self.clock.set(10);
            nested.finish(Outcome::Succeeded);
            Ok(())
        }
    }
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 4);
    let span = profiler.span(None, "outer", SpanKind::Work);
    let stop = span.instrument(Callback {
        clock: clock.clone(),
        profiler: profiler.clone(),
    });
    clock.set(3);
    stop.check().unwrap();
    clock.set(12);
    span.finish(Outcome::Succeeded);
    let trace = profiler.snapshot();
    assert_eq!(trace.spans[1].stats.check_time, Duration::from_millis(7));
    assert_eq!(trace.spans[1].stats.max_check_gap, Duration::from_millis(9));
}

#[test]
fn bounded_retention_and_abandonment_are_visible_and_json_escapes_labels() {
    let profiler = Profiler::new(ManualClock::default(), 1);
    profiler.metadata("key\"\n", "value\t\\\u{0001}😀");
    profiler.metadata("key\"\n", "replacement\t\\\u{0001}😀");
    let first = profiler.span(None, "name\"\n", SpanKind::Work);
    let first_work = first.instrument(NoReport);
    first_work.advance(u64::MAX);
    first_work.advance(1);
    let second = profiler.span(None, "dropped", SpanKind::Work);
    assert_eq!(profiler.try_snapshot().unwrap().active_spans, 2);
    drop(first);
    drop(second);
    first_work.advance(1); // Finished instrumentation is frozen.
    let trace = profiler.snapshot();
    assert_eq!(trace.dropped_spans, 1);
    assert_eq!(trace.active_spans, 0);
    assert_eq!(trace.metadata.len(), 1);
    assert!(trace.spans[0].stats.overflowed);
    assert_eq!(trace.spans[0].outcome, Outcome::Abandoned);
    let mut json = String::new();
    trace.write_json(&mut json).unwrap();
    assert!(json.contains("\"schema_version\":2"));
    assert!(json.contains("replacement\\t\\\\\\u0001😀"));
    assert!(json.contains("\"task\":\"name\\\"\\n\""));
    assert!(!json.contains('\n'));
    assert!(trace.to_string().contains("1 dropped"));
}

#[test]
fn bad_clock_is_diagnostic_and_noop_instrumentation_survives_type_erasure() {
    let clock = ManualClock::default();
    clock.set(20);
    let profiler = Profiler::new(clock.clone(), 1);
    let span = profiler.span(None, "clock failure", SpanKind::Work);
    let meter = span.instrument(Unstoppable);
    assert!(meter.may_stop());
    let stop = almost_enough::StopToken::new(meter);
    clock.set(10);
    stop.check().unwrap();
    clock.set(9);
    span.finish(Outcome::Failed);
    let trace = profiler.snapshot();
    assert_eq!(trace.spans[0].stats.checks, 1);
    // The reading at finish (9) is behind the last check's (20): one regression.
    assert_eq!(trace.spans[0].stats.clock_regressions, 1);
    assert!(trace.overlap(&[trace.spans[0].id]).is_none());
}

#[test]
fn idle_gaps_and_zero_duration_spans_do_not_create_fake_tail_time() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 4);
    let a = profiler.span(None, "first", SpanKind::Work);
    let aid = a.id();
    clock.set(10);
    a.finish(Outcome::Succeeded);
    clock.set(20);
    let b = profiler.span(None, "last", SpanKind::Work);
    let bid = b.id();
    clock.set(25);
    b.finish(Outcome::Succeeded);
    let overlap = profiler.snapshot().overlap(&[aid, bid]).unwrap();
    assert_eq!(overlap.single_task_tail, Duration::from_millis(5));
    assert_eq!(overlap.wall, Duration::from_millis(25));
    let z = profiler.span(None, "zero", SpanKind::Work);
    let zid = z.id();
    z.finish(Outcome::Succeeded);
    let overlap = profiler.snapshot().overlap(&[zid]).unwrap();
    assert_eq!(overlap.wall, Duration::ZERO);
    assert_eq!(overlap.mean_active_tasks, 0.0);
}

#[test]
fn timeout_reason_and_attached_plan_survive_export() {
    struct Timeout;
    impl Stop for Timeout {
        fn check(&self) -> Result<(), StopReason> {
            Err(StopReason::TimedOut)
        }
    }
    let profiler = Profiler::new(ManualClock::default(), 1);
    let span = profiler.span(None, "deadline", SpanKind::Work);
    assert_eq!(span.instrument(Timeout).check(), Err(StopReason::TimedOut));
    span.finish(Outcome::Cancelled);
    let mut phase = how_far_along::Phase::new("job", how_far_along::Total::Estimated(3));
    phase.set_total(how_far_along::Total::Exact(4)).unwrap();
    let trace = profiler
        .snapshot()
        .with_progress(phase.observer().snapshot());
    assert_eq!(trace.spans[0].stats.stop_reason, Some(StopReason::TimedOut));
    let mut json = String::new();
    trace.write_json(&mut json).unwrap();
    assert!(json.contains("\"stop_reason\":\"TimedOut\""));
    assert!(json.contains("\"initial_total\":{\"kind\":\"Estimated\",\"count\":\"3\"}"));
    assert!(json.contains("\"total_revisions\":[{\"kind\":\"Exact\",\"count\":\"4\"}]"));
    // The trace composes the tracker's public versioned JSON without private access.
    assert_eq!(json.matches("schema_version").count(), 2);
}

#[test]
fn reporting_reads_the_clock_only_when_report_timing_is_on() {
    struct CountingClock(Arc<AtomicU64>);
    impl Clock for CountingClock {
        fn now(&self) -> Duration {
            Duration::from_nanos(self.0.fetch_add(1, Ordering::Relaxed))
        }
    }
    let reads = Arc::new(AtomicU64::new(0));
    let profiler = Profiler::new(CountingClock(reads.clone()), 2);
    for timed in [false, true] {
        profiler.set_report_timing(timed);
        let span = profiler.span(None, "clock policy", SpanKind::Work);
        let work = span.instrument(ProgressWithStop::new(Unstoppable, NoReport));
        let before = reads.load(Ordering::Relaxed);
        for _ in 0..100 {
            work.advance(1);
        }
        let report_reads = if timed { 100 } else { 0 };
        assert_eq!(reads.load(Ordering::Relaxed), before + report_reads);
        work.check().unwrap();
        assert_eq!(reads.load(Ordering::Relaxed), before + report_reads + 2);
        span.finish(Outcome::Succeeded);
        assert_eq!(reads.load(Ordering::Relaxed), before + report_reads + 3);
    }
    let trace = profiler.snapshot();
    assert_eq!(trace.spans[0].stats.reports, 100);
    assert!(trace.spans[0].stats.max_report_gap.is_none());
    assert!(trace.spans[1].stats.max_report_gap.is_some());
}

#[test]
fn workers_sharing_a_span_never_look_like_a_backwards_clock() {
    // Each worker reads the clock before taking the span's lock, so readings
    // reach the span out of order. That is concurrency, not a clock fault.
    let profiler = Profiler::new(how_far_really::profile::StdClock::new(), 1);
    profiler.set_report_timing(true);
    let span = profiler.span(None, "shared", SpanKind::Work);
    let work = span.instrument(ProgressWithStop::new(Unstoppable, NoReport));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..2_000 {
                    work.step(1).unwrap();
                }
            });
        }
    });
    span.finish(Outcome::Succeeded);
    let trace = profiler.snapshot();
    assert_eq!(trace.spans[0].stats.clock_regressions, 0);
    assert_eq!(trace.spans[0].stats.checks, 16_000);
    assert_eq!(trace.spans[0].stats.units, 16_000);
}

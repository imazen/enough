#![cfg(feature = "profile")]
use howfar::ext::WorkExt;
use howfar::poll::ControlHandle;
use howfar::profile::{Clock, Profiler, SpanKind};
use howfar::{NoProgress, Outcome, Report, Stop, StopReason, Unstoppable, Work};
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
        .map(|i| profiler.span(7, format!("task-{i}"), SpanKind::Work))
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
    assert!(trace.overlap(&[999]).is_none());
}

#[test]
fn per_task_boundary_gaps_cannot_be_masked_by_another_busy_worker() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 2);
    let busy = profiler.span(1, "frequent", SpanKind::Work);
    let silent = profiler.span(1, "unpolled tail", SpanKind::Work);
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
    let span = profiler.span(0, "candidate search", SpanKind::Work);
    let work = span.instrument(Work::new(Unstoppable, NoProgress));
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
    let check = record
        .stats
        .sites
        .iter()
        .find(|s| s.line == checks_line)
        .unwrap();
    assert_eq!(check.checks, 10_000);
    assert_eq!(
        record
            .stats
            .sites
            .iter()
            .find(|s| s.line == reports_line)
            .unwrap()
            .units,
        16
    );
    let step = record
        .stats
        .sites
        .iter()
        .find(|s| s.line == step_line)
        .unwrap();
    assert_eq!((step.checks, step.reports), (1, 1));
    assert!(
        record
            .stats
            .sites
            .iter()
            .all(|s| s.file.ends_with("tests/profiling.rs"))
    );
}

#[test]
fn cancellation_measurements_include_observation_and_join_cleanup_tail() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 2);
    let span = profiler.span(0, "worker", SpanKind::Work);
    let control = ControlHandle::new();
    let stop = span.instrument(control.clone());
    clock.set(10);
    profiler.cancellation_requested();
    control.cancel();
    clock.set(17);
    assert_eq!(stop.check(), Err(StopReason::Cancelled));
    clock.set(24);
    span.finish(Outcome::Cancelled);
    let join = profiler.span(0, "join and cleanup", SpanKind::Wait);
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
    let outer = profiler.span(1, "outer", SpanKind::Work);
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
            let nested = self.profiler.span(0, "callback", SpanKind::Callback);
            self.profiler.metadata("reentered", "yes");
            self.clock.set(10);
            nested.finish(Outcome::Succeeded);
            Ok(())
        }
    }
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 4);
    let span = profiler.span(0, "outer", SpanKind::Work);
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
    let first = profiler.span(0, "name\"\n", SpanKind::Work);
    let first_work = first.instrument(NoProgress);
    first_work.advance(u64::MAX);
    first_work.advance(1);
    let second = profiler.span(0, "dropped", SpanKind::Work);
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
    assert!(json.contains("\"schema_version\":1"));
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
    let span = profiler.span(0, "clock failure", SpanKind::Work);
    let meter = span.instrument(Unstoppable);
    assert!(meter.may_stop());
    let stop = almost_enough::StopToken::new(meter);
    clock.set(10);
    stop.check().unwrap();
    clock.set(9);
    span.finish(Outcome::Failed);
    let trace = profiler.snapshot();
    assert_eq!(trace.spans[0].stats.checks, 1);
    assert_eq!(trace.spans[0].stats.clock_regressions, 2);
    assert!(trace.overlap(&[trace.spans[0].id]).is_none());
}

#[test]
fn idle_gaps_and_zero_duration_spans_do_not_create_fake_tail_time() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 4);
    let a = profiler.span(0, "first", SpanKind::Work);
    let aid = a.id();
    clock.set(10);
    a.finish(Outcome::Succeeded);
    clock.set(20);
    let b = profiler.span(0, "last", SpanKind::Work);
    let bid = b.id();
    clock.set(25);
    b.finish(Outcome::Succeeded);
    let overlap = profiler.snapshot().overlap(&[aid, bid]).unwrap();
    assert_eq!(overlap.single_task_tail, Duration::from_millis(5));
    assert_eq!(overlap.wall, Duration::from_millis(25));
    let z = profiler.span(0, "zero", SpanKind::Work);
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
    let span = profiler.span(0, "deadline", SpanKind::Work);
    assert_eq!(span.instrument(Timeout).check(), Err(StopReason::TimedOut));
    span.finish(Outcome::Cancelled);
    let mut phase = howfar::Phase::new("job", howfar::Total::Estimated(3));
    phase.set_total(howfar::Total::Exact(4)).unwrap();
    let trace = profiler
        .snapshot()
        .with_progress(phase.observer().snapshot());
    assert_eq!(trace.spans[0].stats.stop_reason, Some(StopReason::TimedOut));
    let mut json = String::new();
    trace.write_json(&mut json).unwrap();
    assert!(json.contains("\"stop_reason\":\"TimedOut\""));
    assert!(json.contains("\"initial_total\":{\"kind\":\"Estimated\",\"units\":\"3\"}"));
    assert!(json.contains("\"total_revisions\":[{\"kind\":\"Exact\",\"units\":\"4\"}]"));
}

#[test]
fn reporting_does_not_read_the_clock_and_span_finish_is_measured_once() {
    struct CountingClock(Arc<AtomicU64>);
    impl Clock for CountingClock {
        fn now(&self) -> Duration {
            Duration::from_nanos(self.0.fetch_add(1, Ordering::Relaxed))
        }
    }
    let reads = Arc::new(AtomicU64::new(0));
    let profiler = Profiler::new(CountingClock(reads.clone()), 1);
    let span = profiler.span(0, "clock policy", SpanKind::Work);
    let work = span.instrument(Work::new(Unstoppable, NoProgress));
    let before = reads.load(Ordering::Relaxed);
    for _ in 0..100 {
        work.advance(1);
    }
    assert_eq!(reads.load(Ordering::Relaxed), before);
    work.check().unwrap();
    assert_eq!(reads.load(Ordering::Relaxed), before + 2);
    span.finish(Outcome::Succeeded);
    assert_eq!(reads.load(Ordering::Relaxed), before + 3);
    assert_eq!(profiler.snapshot().spans[0].stats.reports, 100);
}

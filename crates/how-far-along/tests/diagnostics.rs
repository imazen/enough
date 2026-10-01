#![cfg(feature = "diagnostics")]

use how_far::{PhaseSpec, Pulse, RunError, Steps, StopReason};
use how_far_along::diagnostics::{DiagnosticPulse, Kind, Options};
use how_far_along::poll::{Control, ControlHandle, LocalPoller};
use how_far_along::profile::{Clock, Profiler, SpanKind};
use how_far_along::{
    Execution, IgnoreProgress, Part, Phase, ProgressExt, ProgressWithStop, PulseTree, Report, Stop,
    Total, Unstoppable,
};
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
    fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::Relaxed);
    }
}
impl Clock for ManualClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::Relaxed))
    }
}

#[test]
fn stop_only_users_get_gap_guidance_without_progress_or_tracker_state() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 2);
    let span = profiler.span(0, "decode", SpanKind::Work);
    let stop = span.instrument(Unstoppable);
    clock.set(1);
    stop.check().unwrap();
    clock.set(22);
    span.finish(how_far_along::Outcome::Succeeded);
    let findings = profiler.snapshot().diagnose(&Options::default());
    assert!(
        findings
            .iter()
            .any(|f| f.kind == Kind::StopGap && f.evidence.contains("21.00 ms"))
    );
    assert!(!findings.iter().any(|f| f.kind == Kind::ReportGap));
}

#[test]
fn reports_identify_both_source_lines_and_callback_budget_is_measured() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 5);
    let span = profiler.span(0, "rows", SpanKind::Work);
    let work = span.instrument(ProgressWithStop::new(Unstoppable, IgnoreProgress));
    clock.set(1);
    let first = line!() + 1;
    work.step(1).unwrap();
    clock.set(35);
    let second = line!() + 1;
    work.step(1).unwrap();
    clock.set(36);
    span.finish(how_far_along::Outcome::Succeeded);
    clock.set(40);
    profiler.measure_callback(0, "UI render", || clock.set(52));
    let trace = profiler.snapshot();
    let mut options = Options::default();
    options.report_gap_target = Duration::from_millis(10);
    let findings = trace.diagnose(&options);
    let report = findings.iter().find(|f| f.kind == Kind::ReportGap).unwrap();
    assert!(report.evidence.contains(&format!("{}:{first}", file!())));
    assert!(report.evidence.contains(&format!("{}:{second}", file!())));
    assert!(
        findings
            .iter()
            .any(|f| f.kind == Kind::CallbackDuration && f.evidence.contains("12.00 ms"))
    );
    let mut json = String::new();
    trace.write_json(&mut json).unwrap();
    assert!(json.contains("\"max_report_gap\""));
}

#[test]
fn sequential_stage_weights_produce_a_copyable_candidate_but_parallel_overlap_does_not() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 4);
    let mut root = Phase::new("encode", Total::Unknown);
    let [mut prepare, mut encode] = root
        .split(
            Execution::Sequence,
            [
                Part::new("prepare", 50, Total::Exact(1)).units("rows"),
                Part::new("encode", 50, Total::Exact(1))
                    .execution(Execution::WorkPool { max_parallelism: 4 }),
            ],
        )
        .unwrap();
    let first = profiler.span(prepare.id(), "prepare", SpanKind::Work);
    prepare.progress().advance(1);
    clock.set(10);
    first.finish(how_far_along::Outcome::Succeeded);
    prepare.finish().unwrap();
    let second = profiler.span(encode.id(), "encode", SpanKind::Work);
    encode.progress().advance(1);
    clock.set(100);
    second.finish(how_far_along::Outcome::Succeeded);
    encode.finish().unwrap();
    root.finish().unwrap();
    let trace = profiler
        .snapshot()
        .with_progress(root.observer().snapshot());
    let findings = trace.diagnose(&Options::default());
    let weights = findings
        .iter()
        .find(|f| f.kind == Kind::StageWeights)
        .unwrap();
    let code = weights.sample_code.as_ref().unwrap();
    assert!(code.contains("PhaseSpec::new(\"prepare\", 10, Total::Exact(1)).units(\"rows\")"));
    assert!(code.contains("PhaseSpec::new(\"encode\", 90, Total::Exact(1)).execution(Execution::WorkPool { max_parallelism: 4 })"));

    // The same data cannot justify serial weights if the declared branches overlap.
    let mut overlap = trace.clone();
    overlap.spans[1].start = Duration::from_millis(5);
    assert!(
        !overlap
            .diagnose(&Options::default())
            .iter()
            .any(|f| f.kind == Kind::StageWeights)
    );
}

#[test]
fn report_timing_cost_is_opt_in_and_stop_only_calls_still_have_their_own_cadence() {
    struct CountingClock(Arc<AtomicU64>);
    impl Clock for CountingClock {
        fn now(&self) -> Duration {
            Duration::from_nanos(self.0.fetch_add(1, Ordering::Relaxed))
        }
    }
    let reads = Arc::new(AtomicU64::new(0));
    let profiler = Profiler::new(CountingClock(reads.clone()), 1);
    let span = profiler.span(0, "rows", SpanKind::Work);
    let work = span.instrument(ProgressWithStop::new(Unstoppable, IgnoreProgress));
    let before = reads.load(Ordering::Relaxed);
    for _ in 0..100 {
        work.advance(1);
    }
    assert_eq!(reads.load(Ordering::Relaxed), before + 100);
    work.check().unwrap();
    assert_eq!(reads.load(Ordering::Relaxed), before + 102);
    span.finish(how_far_along::Outcome::Succeeded);
    assert_eq!(reads.load(Ordering::Relaxed), before + 103);
}

#[test]
fn frequent_check_and_report_sites_are_identified_separately() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 1);
    let span = profiler.span(0, "fast loop", SpanKind::Work);
    let work = span.instrument(ProgressWithStop::new(Unstoppable, IgnoreProgress));
    let check_line = line!() + 2;
    for _ in 0..100 {
        work.check().unwrap();
    }
    let report_line = line!() + 2;
    for _ in 0..100 {
        work.advance(1);
    }
    clock.set(1);
    span.finish(how_far_along::Outcome::Succeeded);
    let mut options = Options::default();
    options.check_calls_per_second = 50_000.0;
    options.report_calls_per_second = 50_000.0;
    let findings = profiler.snapshot().diagnose(&options);
    assert!(findings.iter().any(|f| f.kind == Kind::CheckFrequency
        && f.evidence.contains(&format!("{}:{check_line}", file!()))));
    assert!(findings.iter().any(|f| f.kind == Kind::ReportFrequency
        && f.evidence.contains(&format!("{}:{report_line}", file!()))));
}

#[test]
fn an_actual_poller_subscriber_can_time_lazy_snapshot_work() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 2);
    let phase = Phase::new("job", Total::Unknown);
    let mut poller = LocalPoller::new(phase.observer(), ControlHandle::new());
    let measured = profiler.clone();
    poller.subscribe(move |event| {
        measured.measure_callback(0, "UI callback", || {
            assert!(!event.snapshot_materialized());
            clock.set(12);
            assert_eq!(event.snapshot().name, "job");
            Control::Continue
        })
    });
    poller.poll();
    assert!(
        profiler
            .snapshot()
            .diagnose(&Options::default())
            .iter()
            .any(|f| f.kind == Kind::CallbackDuration && f.evidence.contains("12.00 ms"))
    );
}

#[test]
fn callback_cadence_is_measured_separately_from_callback_duration() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 3);
    profiler.measure_callback(0, "UI", || clock.set(1));
    clock.set(15);
    profiler.measure_callback(0, "UI", || clock.set(16));
    let findings = profiler.snapshot().diagnose(&Options::default());
    assert!(
        findings
            .iter()
            .any(|f| f.kind == Kind::CallbackInterval && f.evidence.contains("15.00 ms"))
    );
    assert!(!findings.iter().any(|f| f.kind == Kind::CallbackDuration));
}

#[test]
fn a_library_dyn_pulse_is_instrumented_without_changing_its_signature() {
    fn library(pulse: &dyn Pulse, clock: &ManualClock) -> Result<(), RunError<StopReason>> {
        let mut stages = Steps::new(
            pulse,
            &[
                PhaseSpec::new("prepare", 50, Total::Exact(1)),
                PhaseSpec::new("encode", 50, Total::Exact(1)),
            ],
        )?;
        stages.run_stoppable(|stage| {
            clock.set(1);
            stage.check()?;
            clock.set(10);
            stage.step(1)?;
            Ok(())
        })?;
        stages.run_stoppable(|stage| {
            clock.set(11);
            stage.check()?;
            clock.set(100);
            stage.step(1)?;
            Ok(())
        })?;
        stages.finish()?;
        Ok(())
    }
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 4);
    let pulse = PulseTree::new(Phase::new("job", Total::Unknown), &Unstoppable);
    let observer = pulse.observer();
    let diagnostic = DiagnosticPulse::new(&pulse, observer.clone(), &profiler);
    library(&diagnostic, &clock).unwrap();
    let snapshot = observer.snapshot();
    let trace = profiler.snapshot().with_progress(snapshot.clone());
    assert_eq!(trace.spans.len(), 2);
    assert_eq!(trace.spans[0].node, snapshot.children[0].id);
    assert_eq!(trace.spans[1].node, snapshot.children[1].id);
    assert!(
        trace
            .diagnose(&Options::default())
            .iter()
            .any(|f| f.kind == Kind::StageWeights)
    );
}

#[test]
fn diagnostic_pulse_keeps_nested_child_ids_and_outcomes() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 4);
    let pulse = PulseTree::new(Phase::new("job", Total::Unknown), &Unstoppable);
    let observer = pulse.observer();
    let measured = DiagnosticPulse::new(&pulse, observer.clone(), &profiler);
    let outer = measured
        .split(
            Execution::Sequence,
            &[PhaseSpec::new("middle", 1, Total::Unknown)],
        )
        .unwrap();
    let children = outer[0]
        .split(
            Execution::ForkJoin,
            &[
                PhaseSpec::new("fast", 1, Total::Exact(1)),
                PhaseSpec::new("slow", 1, Total::Exact(1)),
            ],
        )
        .unwrap();
    for (index, child) in children.iter().enumerate() {
        clock.set((index as u64 + 1) * 5);
        child.step(1).unwrap();
        child.finish(how_far_along::Outcome::Succeeded).unwrap();
    }
    outer[0].finish(how_far_along::Outcome::Succeeded).unwrap();
    measured.finish(how_far_along::Outcome::Succeeded).unwrap();
    let snapshot = observer.snapshot();
    let trace = profiler.snapshot();
    assert_eq!(trace.spans.len(), 2);
    assert_eq!(trace.spans[0].node, snapshot.children[0].children[0].id);
    assert_eq!(trace.spans[1].node, snapshot.children[0].children[1].id);
    assert!(
        trace
            .spans
            .iter()
            .all(|span| span.outcome == how_far_along::Outcome::Succeeded)
    );
}

#[test]
fn serial_parallel_serial_weights_use_the_parallel_subtree_wall_window() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 6);
    let pulse = PulseTree::new(Phase::new("job", Total::Unknown), &Unstoppable);
    let observer = pulse.observer();
    let measured = DiagnosticPulse::new(&pulse, observer.clone(), &profiler);
    let stages = measured
        .split(
            Execution::Sequence,
            &[
                PhaseSpec::new("before", 33, Total::Exact(1)),
                PhaseSpec::new("middle", 34, Total::Unknown),
                PhaseSpec::new("after", 33, Total::Exact(1)),
            ],
        )
        .unwrap();
    stages[0].check().unwrap();
    clock.set(10);
    stages[0].step(1).unwrap();
    stages[0].finish(how_far_along::Outcome::Succeeded).unwrap();

    let workers = stages[1]
        .split(
            Execution::ForkJoin,
            &[
                PhaseSpec::new("quick", 1, Total::Exact(1)),
                PhaseSpec::new("slow", 1, Total::Exact(1)),
            ],
        )
        .unwrap();
    workers[0].check().unwrap();
    workers[1].check().unwrap();
    clock.set(30);
    workers[0].step(1).unwrap();
    workers[0]
        .finish(how_far_along::Outcome::Succeeded)
        .unwrap();
    clock.set(80);
    workers[1].step(1).unwrap();
    workers[1]
        .finish(how_far_along::Outcome::Succeeded)
        .unwrap();
    stages[1].finish(how_far_along::Outcome::Succeeded).unwrap();

    stages[2].check().unwrap();
    clock.set(100);
    stages[2].step(1).unwrap();
    stages[2].finish(how_far_along::Outcome::Succeeded).unwrap();
    measured.finish(how_far_along::Outcome::Succeeded).unwrap();
    let trace = profiler.snapshot().with_progress(observer.snapshot());
    let weights = trace
        .diagnose(&Options::default())
        .into_iter()
        .find(|finding| finding.kind == Kind::StageWeights)
        .unwrap();
    assert!(weights.evidence.contains("[10, 70, 20]"));
    assert!(
        weights
            .sample_code
            .unwrap()
            .contains("PhaseSpec::new(\"middle\", 70, Total::Unknown)")
    );
}

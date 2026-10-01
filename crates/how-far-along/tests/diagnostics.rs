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
    assert!(json.contains("\"max_check_gap_start\""));
    assert!(json.contains("\"checks\":0,\"max_check_gap\""));
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

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// Run sequential `(name, weight, end_ms)` stages whose only checkpoint is a
/// `step` at the end, as a library that works first and reports afterwards does.
fn staged(stages: &[(&'static str, u64, u64)]) -> how_far_along::profile::Trace {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 16);
    let pulse = PulseTree::new(Phase::new("job", Total::Unknown), &Unstoppable);
    let observer = pulse.observer();
    let measured = DiagnosticPulse::new(&pulse, observer.clone(), &profiler);
    let specs: Vec<_> = stages
        .iter()
        .map(|(name, weight, _)| PhaseSpec::new(name, *weight, Total::Exact(1)))
        .collect();
    let mut steps = Steps::new(&measured, &specs).unwrap();
    for (_, _, end) in stages {
        steps
            .run_stoppable(|stage| {
                clock.set(*end);
                stage.step(1)
            })
            .unwrap();
    }
    steps.finish().unwrap();
    profiler.snapshot().with_progress(observer.snapshot())
}

#[test]
fn a_sequential_stage_is_timed_from_the_previous_stage_not_its_first_checkpoint() {
    // "flush" does 40 ms of work and only then calls step(1): its first
    // checkpoint is at 130 ms, but the stage was entered at 90 ms.
    let trace = staged(&[("frames", 9, 90), ("flush", 1, 130)]);
    assert_eq!(trace.spans[0].start, ms(0));
    assert_eq!(trace.spans[1].start, ms(90));
    assert_eq!(trace.spans[1].elapsed(), ms(40));
    let findings = trace.diagnose(&Options::default());
    let gap = findings
        .iter()
        .find(|f| f.kind == Kind::StopGap && f.evidence.contains("flush"))
        .expect("pre-checkpoint work is a stop gap");
    assert!(gap.evidence.contains("40.00 ms"));
}

#[test]
fn a_succeeded_leaf_stage_without_any_checkpoint_still_has_a_span() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 4);
    let pulse = PulseTree::new(Phase::new("job", Total::Unknown), &Unstoppable);
    let measured = DiagnosticPulse::new(&pulse, pulse.observer(), &profiler);
    let mut steps = Steps::new(&measured, &[PhaseSpec::new("opaque", 1, Total::Exact(1))]).unwrap();
    steps
        .run_stoppable(|_| {
            clock.set(25);
            Ok::<_, StopReason>(())
        })
        .unwrap();
    steps.finish().unwrap();
    let trace = profiler.snapshot();
    assert_eq!(trace.spans.len(), 1);
    assert_eq!(trace.spans[0].elapsed(), ms(25));
    assert!(
        trace
            .diagnose(&Options::default())
            .iter()
            .any(|f| f.kind == Kind::StopGap && f.evidence.contains("25.00 ms"))
    );
}

#[test]
fn a_near_zero_stage_keeps_its_declared_weight_instead_of_driving_advice() {
    let mut options = Options::default();
    options.weight_difference = 0.05;
    // flush is 0.1% of the run: the old 99/1 candidate was noise-driven.
    let trace = staged(&[("frames", 9, 1000), ("flush", 1, 1001)]);
    assert_eq!(trace.spans[1].elapsed(), ms(1));
    assert!(
        !trace
            .diagnose(&options)
            .iter()
            .any(|f| f.kind == Kind::StageWeights)
    );
    // Disabling the floor restores the noise-driven candidate.
    options.negligible_stage_share = 0.0;
    assert!(
        trace
            .diagnose(&options)
            .iter()
            .any(|f| f.kind == Kind::StageWeights)
    );
    options.negligible_stage_share = Options::default().negligible_stage_share;
    // The same stage, measured at a calibratable share, is still advised on.
    let trace = staged(&[("frames", 9, 1000), ("flush", 1, 1500)]);
    assert!(
        trace
            .diagnose(&options)
            .iter()
            .any(|f| f.kind == Kind::StageWeights)
    );
}

#[test]
fn other_stages_are_recalibrated_around_a_held_negligible_stage() {
    let trace = staged(&[("a", 40, 100), ("b", 50, 1090), ("c", 10, 1100)]);
    let finding = trace
        .diagnose(&Options::default())
        .into_iter()
        .find(|f| f.kind == Kind::StageWeights)
        .unwrap();
    assert!(finding.evidence.contains("keeps its declared share"));
    let code = finding.sample_code.unwrap();
    assert!(code.contains("PhaseSpec::new(\"c\", 10, Total::Exact(1))"));
    assert!(code.contains("PhaseSpec::new(\"b\", 82, Total::Exact(1))"));
}

#[test]
fn a_negligible_stage_that_owns_most_of_the_bar_is_still_reported() {
    let trace = staged(&[("work", 10, 1000), ("tail", 90, 1001)]);
    assert!(
        trace
            .diagnose(&Options::default())
            .iter()
            .any(|f| f.kind == Kind::StageWeights)
    );
}

#[test]
fn a_report_gap_with_frequent_checks_inside_is_a_progress_seam_not_missing_cancellation() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 2);
    let span = profiler.span(0, "frame", SpanKind::Work);
    let work = span.instrument(ProgressWithStop::new(Unstoppable, IgnoreProgress));
    for at in (5..=85).step_by(5) {
        clock.set(at);
        work.check().unwrap();
    }
    work.advance(1);
    span.finish(how_far_along::Outcome::Succeeded);
    let findings = profiler.snapshot().diagnose(&Options::default());
    let report = findings.iter().find(|f| f.kind == Kind::ReportGap).unwrap();
    assert!(report.evidence.contains("85.00 ms"));
    assert!(report.evidence.contains("stop checks inside it: 17"));
    assert!(report.evidence.contains("longest stop gap 5.00 ms"));
    assert!(report.advice.contains("progress-granularity seam"));
    assert!(!findings.iter().any(|f| f.kind == Kind::StopGap));
}

#[test]
fn a_report_gap_with_no_checks_inside_is_also_a_cancellation_gap() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 2);
    let span = profiler.span(0, "frame", SpanKind::Work);
    let work = span.instrument(ProgressWithStop::new(Unstoppable, IgnoreProgress));
    clock.set(85);
    work.advance(1);
    span.finish(how_far_along::Outcome::Succeeded);
    let findings = profiler.snapshot().diagnose(&Options::default());
    let report = findings.iter().find(|f| f.kind == Kind::ReportGap).unwrap();
    assert!(report.evidence.contains("stop checks inside it: 0"));
    assert!(report.advice.contains("No stop check"));
    assert!(findings.iter().any(|f| f.kind == Kind::StopGap));
}

#[test]
fn checks_recorded_by_a_separate_span_on_the_same_profiler_cover_a_coarse_stage_gap() {
    // The library encodes a frame with its own 'static Stop, so the progress
    // pulse never sees those checks. Instrument that Stop from the same profiler.
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 4);
    let stage = profiler.span(0, "encode frames", SpanKind::Work);
    let stage_pulse = stage.instrument(ProgressWithStop::new(Unstoppable, IgnoreProgress));
    stage_pulse.check().unwrap();
    clock.set(1);
    let inner = profiler.span(99, "raw frame encode", SpanKind::Work);
    let inner_stop = inner.instrument(Unstoppable);
    for at in (3..=83).step_by(2) {
        clock.set(at);
        inner_stop.check().unwrap();
    }
    clock.set(84);
    inner.finish(how_far_along::Outcome::Succeeded);
    clock.set(85);
    stage_pulse.advance(1);
    stage.finish(how_far_along::Outcome::Succeeded);
    let findings = profiler.snapshot().diagnose(&Options::default());
    let stop = findings
        .iter()
        .find(|f| f.kind == Kind::StopGap && f.evidence.contains("encode frames"))
        .unwrap();
    assert!(stop.evidence.contains("raw frame encode"));
    assert!(stop.advice.contains("already covered"));
    let report = findings.iter().find(|f| f.kind == Kind::ReportGap).unwrap();
    assert!(report.advice.contains("progress-granularity seam"));
    assert!(
        !findings
            .iter()
            .any(|f| f.kind == Kind::StopGap && f.evidence.contains("raw frame encode\": "))
    );
}

#[test]
fn a_task_that_does_not_cover_the_interval_is_not_credited() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 4);
    let stage = profiler.span(0, "encode frames", SpanKind::Work);
    let stage_pulse = stage.instrument(Unstoppable);
    stage_pulse.check().unwrap();
    let brief = profiler.span(99, "brief", SpanKind::Work);
    let brief_stop = brief.instrument(Unstoppable);
    clock.set(2);
    brief_stop.check().unwrap();
    brief.finish(how_far_along::Outcome::Succeeded);
    clock.set(85);
    stage_pulse.check().unwrap();
    stage.finish(how_far_along::Outcome::Succeeded);
    let findings = profiler.snapshot().diagnose(&Options::default());
    let stop = findings.iter().find(|f| f.kind == Kind::StopGap).unwrap();
    assert!(!stop.evidence.contains("brief"));
    assert!(!stop.advice.contains("already covered"));
}

#[test]
fn diagnostic_pulse_keeps_checkpoints_visible_over_a_no_op_pulse() {
    let profiler = Profiler::new(ManualClock::default(), 1);
    let phase = Phase::new("job", Total::Unknown);
    let measured = DiagnosticPulse::new(&how_far::NoPulse, phase.observer(), &profiler);
    assert!(measured.may_stop());
    assert!(measured.may_report());
}

#[test]
fn a_stop_adapter_keeps_the_library_call_site() {
    // A library that owns a `'static` Stop from another trait version needs a
    // small adapter around an Instrumented value. Stop::check is declared
    // #[track_caller], so the adapter needs no attribute of its own.
    struct Adapter(how_far_along::profile::Instrumented<Unstoppable>);
    impl Stop for Adapter {
        fn check(&self) -> Result<(), StopReason> {
            self.0.check()
        }
    }
    fn library_loop(stop: &dyn Stop) -> (u32, u32) {
        let first = line!() + 1;
        stop.check().unwrap();
        let second = line!() + 1;
        stop.check().unwrap();
        (first, second)
    }
    let profiler = Profiler::new(ManualClock::default(), 1);
    let span = profiler.span(0, "adapter", SpanKind::Work);
    let (first, second) = library_loop(&Adapter(span.instrument(Unstoppable)));
    span.finish(how_far_along::Outcome::Succeeded);
    let lines: Vec<_> = profiler.snapshot().spans[0]
        .stats
        .sites
        .iter()
        .map(|s| s.line)
        .collect();
    assert_eq!(lines, [first, second]);
}

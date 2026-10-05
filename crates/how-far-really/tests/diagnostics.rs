//! Checkpoint advice from deterministic, clock-driven traces.

use how_far::{Complete, NoReport, Phases, ProgressWithStop, RunError, Stages, TryStages};
use how_far_along::poll::LocalPoller;
use how_far_along::{
    Execution, Outcome, Phase, PhaseSpec, ProgressExt, Pulse, PulseTree, Report, Stop, StopReason,
    Total, Unstoppable,
};
use how_far_really::diagnostics::{DiagnosticPulse, Kind, Options};
use how_far_really::profile::{Clock, Profiler, SpanKind, Trace};
use std::{
    num::NonZeroUsize,
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

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn tree() -> PulseTree {
    PulseTree::new(Phase::new("job", Total::Unknown), Unstoppable)
}

#[test]
fn stop_only_code_gets_gap_advice_without_a_progress_tree() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 2);
    let span = profiler.span(None, "decode", SpanKind::Work);
    let stop = span.instrument(Unstoppable);
    clock.set(1);
    stop.check().unwrap();
    clock.set(22);
    span.finish(Outcome::Succeeded);
    let findings = profiler.snapshot().diagnose(&Options::default());
    assert!(
        findings
            .iter()
            .any(|f| f.kind == Kind::StopGap && f.evidence.contains("21.00 ms"))
    );
    assert!(!findings.iter().any(|f| f.kind == Kind::ReportGap));
}

#[test]
fn report_gaps_name_both_source_lines_and_callback_budgets_are_measured() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 5);
    profiler.set_report_timing(true);
    let span = profiler.span(None, "rows", SpanKind::Work);
    let work = span.instrument(ProgressWithStop::new(Unstoppable, NoReport));
    clock.set(1);
    let first = line!() + 1;
    work.step(1).unwrap();
    clock.set(35);
    let second = line!() + 1;
    work.step(1).unwrap();
    clock.set(36);
    span.finish(Outcome::Succeeded);
    clock.set(40);
    profiler.measure_callback("UI render", || clock.set(52));
    let trace = profiler.snapshot();
    let mut options = Options::default();
    options.report_gap_target = ms(10);
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
    assert!(json.contains("\"max_report_gap\":{"));
    assert!(json.contains("\"max_check_gap_start\""));
    assert!(json.contains("\"kind\":\"Callback\""));
}

#[test]
fn sequential_weights_produce_a_copyable_candidate_but_overlapping_branches_do_not() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 4);
    let mut root = Phase::new("encode", Total::Unknown);
    let [mut prepare, mut encode] = root
        .split(
            Execution::Sequence,
            [
                PhaseSpec::new("prepare", 50, Total::Exact(1)).units("rows"),
                PhaseSpec::new("encode", 50, Total::Exact(1))
                    .execution(Execution::work_pool(NonZeroUsize::new(4).unwrap())),
            ],
        )
        .unwrap();
    let first = profiler.span(prepare.id(), "prepare", SpanKind::Work);
    prepare.reporter().advance(1);
    clock.set(10);
    first.finish(Outcome::Succeeded);
    prepare.finish().unwrap();
    let second = profiler.span(encode.id(), "encode", SpanKind::Work);
    encode.reporter().advance(1);
    clock.set(100);
    second.finish(Outcome::Succeeded);
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
    assert!(code.contains("PhaseSpec::new(\"encode\", 90, Total::Exact(1)).execution(Execution::work_pool(NonZeroUsize::new(4).unwrap()))"));

    // The same data cannot justify serial weights if the declared stages overlap.
    let mut overlap = trace.clone();
    overlap.spans[1].start = ms(5);
    assert!(
        !overlap
            .diagnose(&Options::default())
            .iter()
            .any(|f| f.kind == Kind::StageWeights)
    );
}

#[test]
fn frequent_check_and_report_sites_are_identified_separately() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 1);
    let span = profiler.span(None, "fast loop", SpanKind::Work);
    let work = span.instrument(ProgressWithStop::new(Unstoppable, NoReport));
    let check_line = line!() + 2;
    for _ in 0..100 {
        work.check().unwrap();
    }
    let report_line = line!() + 2;
    for _ in 0..100 {
        work.advance(1);
    }
    clock.set(1);
    span.finish(Outcome::Succeeded);
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
fn a_poller_callback_can_time_its_lazy_snapshot_work() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 2);
    let phase = Phase::new("job", Total::Unknown);
    let mut poller = LocalPoller::new(phase.observer());
    let measured = profiler.clone();
    poller.subscribe(move |event| {
        measured.measure_callback("UI callback", || {
            assert!(!event.snapshot_materialized());
            clock.set(12);
            assert_eq!(event.snapshot().name, "job");
        });
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
    profiler.measure_callback("UI", || clock.set(1));
    clock.set(15);
    profiler.measure_callback("UI", || clock.set(16));
    let findings = profiler.snapshot().diagnose(&Options::default());
    assert!(
        findings
            .iter()
            .any(|f| f.kind == Kind::CallbackInterval && f.evidence.contains("15.00 ms"))
    );
    assert!(!findings.iter().any(|f| f.kind == Kind::CallbackDuration));
}

#[test]
fn a_library_is_measured_without_changing_its_signature() {
    fn library(pulse: &dyn Pulse, clock: &ManualClock) -> Result<(), RunError<StopReason>> {
        let mut stages = TryStages::new(
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
            stage.step(1)
        })?;
        stages.run_stoppable(|stage| {
            clock.set(11);
            stage.check()?;
            clock.set(100);
            stage.step(1)
        })?;
        stages.finish()?;
        Ok(())
    }
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 4);
    let measured = DiagnosticPulse::new(tree(), &profiler);
    let observer = measured.observer();
    library(&measured, &clock).unwrap();
    measured.finish(Outcome::Succeeded).unwrap();
    let snapshot = observer.snapshot();
    let trace = profiler.snapshot().with_progress(snapshot.clone());
    assert_eq!(trace.spans.len(), 2);
    assert_eq!(trace.spans[0].node, Some(snapshot.children[0].id));
    assert_eq!(trace.spans[1].node, Some(snapshot.children[1].id));
    assert!(
        trace
            .diagnose(&Options::default())
            .iter()
            .any(|f| f.kind == Kind::StageWeights)
    );
}

#[test]
fn nested_children_keep_their_node_ids_and_outcomes() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 4);
    let measured = DiagnosticPulse::new(tree(), &profiler);
    let observer = measured.observer();
    let [middle] = measured
        .split_array(
            Execution::Sequence,
            [PhaseSpec::new("middle", 1, Total::Unknown)],
        )
        .unwrap();
    let children = middle
        .split(
            Execution::ForkJoin,
            &[
                PhaseSpec::new("fast", 1, Total::Exact(1)),
                PhaseSpec::new("slow", 1, Total::Exact(1)),
            ],
        )
        .unwrap();
    for (index, child) in children.into_iter().enumerate() {
        clock.set((index as u64 + 1) * 5);
        child.step(1).unwrap();
        child.finish(Outcome::Succeeded).unwrap();
    }
    middle.finish(Outcome::Succeeded).unwrap();
    measured.finish(Outcome::Succeeded).unwrap();
    let snapshot = observer.snapshot();
    let trace = profiler.snapshot();
    // Each child's work, then the branch's coordination while they ran. The
    // branch split on entry, so it has no work segment of its own.
    assert_eq!(trace.spans.len(), 3);
    assert_eq!(
        trace.spans[0].node,
        Some(snapshot.children[0].children[0].id)
    );
    assert_eq!(
        trace.spans[1].node,
        Some(snapshot.children[0].children[1].id)
    );
    assert_eq!(trace.spans[2].node, Some(snapshot.children[0].id));
    assert_eq!(trace.spans[2].kind, SpanKind::Wait);
    assert!(
        trace.spans[..2]
            .iter()
            .all(|span| span.kind == SpanKind::Work)
    );
    assert!(
        trace
            .spans
            .iter()
            .all(|span| span.outcome == Outcome::Succeeded)
    );
}

#[test]
fn serial_parallel_serial_weights_use_the_parallel_subtree_wall_window() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 6);
    let measured = DiagnosticPulse::new(tree(), &profiler);
    let observer = measured.observer();
    let [before, middle, after] = measured
        .split_array(
            Execution::Sequence,
            [
                PhaseSpec::new("before", 33, Total::Exact(1)),
                PhaseSpec::new("middle", 34, Total::Unknown),
                PhaseSpec::new("after", 33, Total::Exact(1)),
            ],
        )
        .unwrap();
    before.check().unwrap();
    clock.set(10);
    before.step(1).unwrap();
    before.finish(Outcome::Succeeded).unwrap();

    let [quick, slow] = middle
        .split_array(
            Execution::ForkJoin,
            [
                PhaseSpec::new("quick", 1, Total::Exact(1)),
                PhaseSpec::new("slow", 1, Total::Exact(1)),
            ],
        )
        .unwrap();
    quick.check().unwrap();
    slow.check().unwrap();
    clock.set(30);
    quick.step(1).unwrap();
    quick.finish(Outcome::Succeeded).unwrap();
    clock.set(80);
    slow.step(1).unwrap();
    slow.finish(Outcome::Succeeded).unwrap();
    middle.finish(Outcome::Succeeded).unwrap();

    after.check().unwrap();
    clock.set(100);
    after.step(1).unwrap();
    after.finish(Outcome::Succeeded).unwrap();
    measured.finish(Outcome::Succeeded).unwrap();
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

/// Run sequential `(name, weight, end_ms)` stages whose only checkpoint is a
/// `step` at the end, as a library that works first and reports afterwards does.
fn staged(stages: &[(&'static str, u64, u64)]) -> Trace {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 16);
    let measured = DiagnosticPulse::new(tree(), &profiler);
    let observer = measured.observer();
    let specs: Vec<_> = stages
        .iter()
        .map(|(name, weight, _)| PhaseSpec::new(name, *weight, Total::Exact(1)))
        .collect();
    let mut run = TryStages::new(&measured, &specs).unwrap();
    for (_, _, end) in stages {
        run.run_stoppable(|stage| {
            clock.set(*end);
            stage.step(1)
        })
        .unwrap();
    }
    run.finish().unwrap();
    measured.finish(Outcome::Succeeded).unwrap();
    profiler.snapshot().with_progress(observer.snapshot())
}

#[test]
fn a_sequential_stage_is_timed_from_the_previous_stage_not_its_first_checkpoint() {
    // "flush" works for 40 ms and only then calls step(1): its first
    // checkpoint is at 130 ms, but the stage began at 90 ms.
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
fn a_succeeded_stage_without_any_checkpoint_still_has_a_span() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 4);
    let measured = DiagnosticPulse::new(tree(), &profiler);
    let mut stages =
        TryStages::new(&measured, &[PhaseSpec::new("opaque", 1, Total::Exact(1))]).unwrap();
    stages
        .run_stoppable(|_| {
            clock.set(25);
            Ok::<_, StopReason>(())
        })
        .unwrap();
    stages.finish().unwrap();
    measured.finish(Outcome::Succeeded).unwrap();
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
    // flush took 0.1% of the run; a 99/1 candidate would be noise.
    let trace = staged(&[("frames", 9, 1000), ("flush", 1, 1001)]);
    assert_eq!(trace.spans[1].elapsed(), ms(1));
    assert!(
        !trace
            .diagnose(&options)
            .iter()
            .any(|f| f.kind == Kind::StageWeights)
    );
    // Without the floor, the noise-driven candidate comes back.
    options.negligible_stage_share = 0.0;
    assert!(
        trace
            .diagnose(&options)
            .iter()
            .any(|f| f.kind == Kind::StageWeights)
    );
    options.negligible_stage_share = Options::default().negligible_stage_share;
    // The same stage at a measurable share still gets advice.
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
fn a_report_gap_with_frequent_checks_inside_is_a_reporting_seam_not_missing_cancellation() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 2);
    profiler.set_report_timing(true);
    let span = profiler.span(None, "frame", SpanKind::Work);
    let work = span.instrument(ProgressWithStop::new(Unstoppable, NoReport));
    for at in (5..=85).step_by(5) {
        clock.set(at);
        work.check().unwrap();
    }
    work.advance(1);
    span.finish(Outcome::Succeeded);
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
    profiler.set_report_timing(true);
    let span = profiler.span(None, "frame", SpanKind::Work);
    let work = span.instrument(ProgressWithStop::new(Unstoppable, NoReport));
    clock.set(85);
    work.advance(1);
    span.finish(Outcome::Succeeded);
    let findings = profiler.snapshot().diagnose(&Options::default());
    let report = findings.iter().find(|f| f.kind == Kind::ReportGap).unwrap();
    assert!(report.evidence.contains("stop checks inside it: 0"));
    assert!(report.advice.contains("No stop check"));
    assert!(findings.iter().any(|f| f.kind == Kind::StopGap));
}

#[test]
fn checks_in_a_separate_span_on_the_same_profiler_cover_a_coarse_stage_gap() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 4);
    profiler.set_report_timing(true);
    let stage = profiler.span(None, "encode frames", SpanKind::Work);
    let stage_pulse = stage.instrument(ProgressWithStop::new(Unstoppable, NoReport));
    stage_pulse.check().unwrap();
    clock.set(1);
    let inner = profiler.span(None, "raw frame encode", SpanKind::Work);
    let inner_stop = inner.instrument(Unstoppable);
    for at in (3..=83).step_by(2) {
        clock.set(at);
        inner_stop.check().unwrap();
    }
    clock.set(84);
    inner.finish(Outcome::Succeeded);
    clock.set(85);
    stage_pulse.advance(1);
    stage.finish(Outcome::Succeeded);
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
    let stage = profiler.span(None, "encode frames", SpanKind::Work);
    let stage_pulse = stage.instrument(Unstoppable);
    stage_pulse.check().unwrap();
    let brief = profiler.span(None, "brief", SpanKind::Work);
    let brief_stop = brief.instrument(Unstoppable);
    clock.set(2);
    brief_stop.check().unwrap();
    brief.finish(Outcome::Succeeded);
    clock.set(85);
    stage_pulse.check().unwrap();
    stage.finish(Outcome::Succeeded);
    let findings = profiler.snapshot().diagnose(&Options::default());
    let stop = findings.iter().find(|f| f.kind == Kind::StopGap).unwrap();
    assert!(!stop.evidence.contains("brief"));
    assert!(!stop.advice.contains("already covered"));
}

/// A codec context that, like many encoders, owns its stop policy.
struct Encoder<S: Stop + 'static> {
    stop: S,
}
impl<S: Stop + 'static> Encoder<S> {
    fn encode_frame(&self, clock: &ManualClock, from: u64) -> Result<(), StopReason> {
        for at in (from + 2..from + 85).step_by(2) {
            clock.set(at);
            self.stop.check()?;
        }
        clock.set(from + 85);
        Ok(())
    }
}

#[test]
fn checks_inside_a_codec_that_owns_its_stop_count_toward_the_stage() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 8);
    let measured = DiagnosticPulse::new(tree(), &profiler);
    let observer = measured.observer();
    let mut stages = TryStages::new(
        &measured,
        &[PhaseSpec::new("frames", 1, Total::Exact(2)).units("frames")],
    )
    .unwrap();
    stages
        .run_stoppable(|stage| {
            // The codec context is built once and owns a `'static` stop.
            let encoder = Encoder {
                stop: stage.handle().stop,
            };
            stage.check()?;
            for frame in 0..2 {
                encoder.encode_frame(&clock, frame * 85)?;
                stage.step(1)?;
            }
            Ok::<_, StopReason>(())
        })
        .unwrap();
    stages.finish().unwrap();
    measured.finish(Outcome::Succeeded).unwrap();
    let trace = profiler.snapshot().with_progress(observer.snapshot());
    let frames = &trace.spans[0];
    assert_eq!(frames.task, "frames");
    assert!(frames.stats.checks > 80, "{}", frames.stats.checks);
    let findings = trace.diagnose(&Options::default());
    assert!(
        !findings.iter().any(|f| f.kind == Kind::StopGap),
        "{findings:#?}"
    );
    let report = findings.iter().find(|f| f.kind == Kind::ReportGap).unwrap();
    assert!(report.advice.contains("progress-granularity seam"));
}

#[test]
fn a_diagnostic_pulse_keeps_checkpoints_visible_over_a_never_stopping_tree() {
    let profiler = Profiler::new(ManualClock::default(), 1);
    let plain = tree();
    assert!(!plain.may_stop());
    let measured = DiagnosticPulse::new(plain, &profiler);
    assert!(measured.may_stop());
    assert!(measured.may_report());
    // So a library that gates its hot loop is still measured.
    assert!(measured.live().is_some());
}

#[test]
fn a_stop_adapter_keeps_the_library_call_site() {
    // An adapter around an instrumented value needs no #[track_caller] of its
    // own, because `Stop::check` is declared with it.
    struct Adapter(how_far_really::profile::Instrumented<Unstoppable>);
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
    let span = profiler.span(None, "adapter", SpanKind::Work);
    let (first, second) = library_loop(&Adapter(span.instrument(Unstoppable)));
    span.finish(Outcome::Succeeded);
    let lines: Vec<_> = profiler.snapshot().spans[0]
        .stats
        .sites
        .iter()
        .map(|s| s.line)
        .collect();
    assert_eq!(lines, [first, second]);
}

#[test]
fn report_counts_reach_the_tree_through_the_wrapper() {
    let profiler = Profiler::new(ManualClock::default(), 4);
    let measured = DiagnosticPulse::new(
        PulseTree::new(Phase::new("job", Total::Exact(3)), Unstoppable),
        &profiler,
    );
    let observer = measured.observer();
    measured.step(2).unwrap();
    measured.handle().advance(1);
    measured.finish(Outcome::Succeeded).unwrap();
    assert_eq!(observer.snapshot().completed, 3);
    assert_eq!(profiler.snapshot().spans[0].stats.units, 3);
}

fn diagnose(measured: DiagnosticPulse, profiler: &Profiler) -> (Trace, Vec<Kind>) {
    let observer = measured.observer();
    measured.finish(Outcome::Succeeded).unwrap();
    let trace = profiler.snapshot().with_progress(observer.snapshot());
    let kinds = trace
        .diagnose(&Options::default())
        .iter()
        .map(|f| f.kind)
        .collect();
    (trace, kinds)
}

fn stage_weights(trace: &Trace) -> Option<String> {
    trace
        .diagnose(&Options::default())
        .into_iter()
        .find(|f| f.kind == Kind::StageWeights)
        .map(|f| f.evidence)
}

#[test]
fn audit_nested_stage_includes_coordination_after_children() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 16);
    let measured = DiagnosticPulse::new(tree(), &profiler);
    let observer = measured.observer();
    let [before, middle, after] = measured
        .split_array(
            Execution::Sequence,
            [
                PhaseSpec::new("before", 33, Total::Exact(1)),
                PhaseSpec::new("middle", 34, Total::Unknown),
                PhaseSpec::new("after", 33, Total::Exact(1)),
            ],
        )
        .unwrap();
    before.check().unwrap();
    clock.set(10);
    before.step(1).unwrap();
    before.finish(Outcome::Succeeded).unwrap();

    let [quick, slow] = middle
        .split_array(
            Execution::ForkJoin,
            [
                PhaseSpec::new("quick", 1, Total::Exact(1)),
                PhaseSpec::new("slow", 1, Total::Exact(1)),
            ],
        )
        .unwrap();
    quick.check().unwrap();
    slow.check().unwrap();
    clock.set(30);
    quick.step(1).unwrap();
    quick.finish(Outcome::Succeeded).unwrap();
    clock.set(80);
    slow.step(1).unwrap();
    slow.finish(Outcome::Succeeded).unwrap();
    clock.set(90); // ten ms of parent coordination after the workers finish
    middle.finish(Outcome::Succeeded).unwrap();

    after.check().unwrap();
    clock.set(100);
    after.step(1).unwrap();
    after.finish(Outcome::Succeeded).unwrap();
    measured.finish(Outcome::Succeeded).unwrap();
    let trace = profiler.snapshot().with_progress(observer.snapshot());
    let weights = trace
        .diagnose(&Options::default())
        .into_iter()
        .find(|finding| finding.kind == Kind::StageWeights)
        .unwrap();
    assert!(
        weights.evidence.contains("[10, 80, 10]"),
        "{}",
        weights.evidence
    );
    assert!(
        weights
            .sample_code
            .unwrap()
            .contains("PhaseSpec::new(\"middle\", 80, Total::Unknown)")
    );
}

#[test]
fn a_root_that_checks_on_entry_and_then_splits_is_not_silent() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 8);
    let measured = DiagnosticPulse::new(tree(), &profiler);
    let pulse: &dyn Pulse = &measured;
    pulse.check().unwrap();
    let mut t = 0;
    Stages::new(
        pulse,
        &[
            PhaseSpec::new("a", 1, Total::Exact(100)),
            PhaseSpec::new("b", 1, Total::Exact(100)),
        ],
    )
    .complete_with(|stages| {
        for _ in 0..2 {
            stages.run(|stage| {
                for _ in 0..100 {
                    t += 1;
                    clock.set(t);
                    stage.step(1)?;
                }
                Ok::<_, StopReason>(())
            })?;
        }
        Ok::<_, StopReason>(())
    })
    .unwrap();
    let (_, kinds) = diagnose(measured, &profiler);
    assert!(!kinds.contains(&Kind::StopGap), "{kinds:?}");
}

#[test]
fn held_negligible_stages_cannot_hide_a_misweighted_bar() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 8);
    let measured = DiagnosticPulse::new(tree(), &profiler);
    let mut stages = Stages::new(
        &measured,
        &[
            PhaseSpec::new("a", 45, Total::Exact(1)),
            PhaseSpec::new("b", 45, Total::Exact(1)),
            PhaseSpec::new("c", 10, Total::Exact(1)),
        ],
    );
    for end in [10, 20, 1000] {
        stages
            .run(|stage| {
                clock.set(end);
                stage.step(1)
            })
            .unwrap();
    }
    stages.complete_as(Outcome::Succeeded);
    let (trace, _) = diagnose(measured, &profiler);
    // Held stages may own at most half the bar together, so only one 45%
    // stage keeps its share and the misweighting is still reported.
    let evidence = stage_weights(&trace).expect("90% of the bar on 2% of the time");
    assert!(evidence.contains("[45, 1, 54]"), "{evidence}");
}

#[test]
fn a_branch_stage_is_timed_from_entry_to_exit() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 8);
    let measured = DiagnosticPulse::new(tree(), &profiler);
    let mut stages = Stages::new(
        &measured,
        &[
            PhaseSpec::new("a", 50, Total::Unknown),
            PhaseSpec::new("b", 50, Total::Exact(1)),
        ],
    );
    stages
        .run(|stage| {
            clock.set(30); // setup without a checkpoint, then fork-join
            let parts = [
                PhaseSpec::new("x", 1, Total::Exact(1)),
                PhaseSpec::new("y", 1, Total::Exact(1)),
            ];
            let workers = stage.plan(Execution::ForkJoin, &parts);
            clock.set(40);
            for worker in workers {
                worker.step(1)?;
                worker.finish(Outcome::Succeeded).unwrap();
            }
            Ok::<_, StopReason>(())
        })
        .unwrap();
    stages
        .run(|stage| {
            clock.set(80);
            stage.step(1)
        })
        .unwrap();
    stages.complete_as(Outcome::Succeeded);
    let (trace, kinds) = diagnose(measured, &profiler);
    // 40 ms each, as planned.
    assert_eq!(stage_weights(&trace), None);
    // The 30 ms of setup before the split is the stage's own silence.
    let gap = trace
        .diagnose(&Options::default())
        .into_iter()
        .find(|f| f.kind == Kind::StopGap)
        .expect("setup without a check");
    assert!(gap.evidence.contains("\"a\": 30.00 ms"), "{}", gap.evidence);
    assert!(!kinds.contains(&Kind::IncompleteEvidence));
}

#[test]
fn a_view_retained_past_its_phase_records_nothing_and_leaves_no_open_span() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 8);
    let measured = DiagnosticPulse::new(tree(), &profiler);
    let mut kept = None;
    let mut stages = Stages::new(
        &measured,
        &[
            PhaseSpec::new("a", 1, Total::Exact(1)),
            PhaseSpec::new("b", 1, Total::Exact(1)),
        ],
    );
    stages
        .run(|stage| {
            kept = Some(stage.share().unwrap());
            clock.set(20);
            stage.step(1)
        })
        .unwrap();
    // A codec context keeps its owned view and asks for a legacy handle later.
    let late = kept.as_ref().unwrap().handle();
    late.check().unwrap();
    kept.as_ref().unwrap().check().unwrap();
    stages
        .run(|stage| {
            clock.set(80);
            stage.step(1)
        })
        .unwrap();
    stages.complete_as(Outcome::Succeeded);
    let (trace, kinds) = diagnose(measured, &profiler);
    assert_eq!(trace.active_spans, 0);
    assert!(!kinds.contains(&Kind::IncompleteEvidence), "{kinds:?}");
    assert!(
        stage_weights(&trace).is_some(),
        "20/60 against a 50/50 plan"
    );
    let a = trace.spans.iter().find(|s| s.task == "a").unwrap();
    assert_eq!(a.stats.checks, 1, "late checks are not attributed to a");
}

#[test]
fn a_leaf_that_checks_but_never_reports_has_a_report_gap() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 8);
    let measured = DiagnosticPulse::new(tree(), &profiler);
    let mut phases = Phases::new(
        &measured,
        Execution::ForkJoin,
        &[PhaseSpec::new("a", 1, Total::Unknown)],
    );
    phases
        .run(0, |phase| {
            for t in 0..2000 {
                clock.set(t);
                phase.check()?;
            }
            Ok::<_, StopReason>(())
        })
        .unwrap();
    phases.complete_as(Outcome::Succeeded);
    let (_, kinds) = diagnose(measured, &profiler);
    assert!(kinds.contains(&Kind::ReportGap), "{kinds:?}");
    // It checked every millisecond: only the progress granularity is coarse.
    assert!(!kinds.contains(&Kind::StopGap), "{kinds:?}");
}

#[test]
fn a_stage_within_one_clock_tick_still_gets_weight_advice() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 8);
    let measured = DiagnosticPulse::new(tree(), &profiler);
    let mut stages = Stages::new(
        &measured,
        &[
            PhaseSpec::new("frames", 10, Total::Exact(1)),
            PhaseSpec::new("flush", 90, Total::Exact(1)),
        ],
    );
    for end in [1000, 1000] {
        stages
            .run(|stage| {
                clock.set(end);
                stage.step(1)
            })
            .unwrap();
    }
    stages.complete_as(Outcome::Succeeded);
    let (trace, _) = diagnose(measured, &profiler);
    let evidence = stage_weights(&trace).expect("flush owns 90% of the bar but no time");
    assert!(evidence.contains("[99, 1]"), "{evidence}");
}

#[test]
fn a_cancellation_after_the_operation_returned_is_not_unobserved() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 4);
    let span = profiler.span(None, "op", SpanKind::Work);
    let stop = span.instrument(Unstoppable);
    stop.check().unwrap();
    clock.set(5);
    span.finish(Outcome::Succeeded);
    profiler.operation_returned();
    clock.set(50);
    profiler.cancellation_requested();
    let kinds: Vec<_> = profiler
        .snapshot()
        .diagnose(&Options::default())
        .iter()
        .map(|f| f.kind)
        .collect();
    assert!(!kinds.contains(&Kind::UnobservedCancellation), "{kinds:?}");
}

#[test]
fn callback_cadence_ignores_idle_time_between_operations() {
    let clock = ManualClock::default();
    let profiler = Profiler::new(clock.clone(), 16);
    for base in [0_u64, 1000] {
        clock.set(base);
        let work = profiler.span(None, "operation", SpanKind::Work);
        for i in 0..4 {
            clock.set(base + i * 5);
            profiler.measure_callback("render", || ());
        }
        work.finish(Outcome::Succeeded);
    }
    let kinds: Vec<_> = profiler
        .snapshot()
        .diagnose(&Options::default())
        .iter()
        .map(|f| f.kind)
        .collect();
    assert!(!kinds.contains(&Kind::CallbackInterval), "{kinds:?}");
}

//! `FnPulse`: one callback that sees every report and can stop the work.

use how_far::{
    Execution, FnPulse, Outcome, PhaseSpec, PlanError, Progress, ProgressExt, Pulse, Report,
    RunError, Stages, Stop, StopReason, Total,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// One recorded callback.
#[derive(Clone, Debug, PartialEq)]
struct Seen {
    fraction: f64,
    phase: String,
    units: String,
    completed: u64,
    total: Total,
    outcome: Option<Outcome>,
}

impl From<&Progress<'_>> for Seen {
    fn from(p: &Progress<'_>) -> Self {
        Seen {
            fraction: p.fraction,
            phase: p.phase.into(),
            units: p.units.into(),
            completed: p.completed,
            total: p.total,
            outcome: p.outcome,
        }
    }
}

/// A pulse that records every report, and stops once `stop_at` says so after
/// one. Checks pass.
fn recording(
    stop_at: impl Fn(&Progress<'_>) -> Option<StopReason> + Send + Sync + 'static,
) -> (FnPulse, Arc<Mutex<Vec<Seen>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&log);
    let pulse = FnPulse::new(move |progress| match progress {
        Some(progress) => {
            sink.lock().unwrap().push(Seen::from(progress));
            stop_at(progress).map_or(Ok(()), Err)
        }
        None => Ok(()),
    });
    (pulse, log)
}

/// A library that plans two weighted stages and counts rows in each.
fn library(pulse: &dyn Pulse, rows: u64) -> Result<(), RunError<StopReason>> {
    let mut stages = Stages::new(
        pulse,
        &[
            PhaseSpec::new("decode", 1, Total::Exact(rows)).units("rows"),
            PhaseSpec::new("encode", 3, Total::Exact(rows)).units("rows"),
        ],
    )?;
    for _ in 0..2 {
        stages.run_stoppable(|stage| {
            stage.check()?;
            for _ in 0..rows {
                stage.step(1)?;
            }
            Ok(())
        })?;
    }
    stages.finish()?;
    Ok(())
}

#[test]
fn the_callback_sees_weighted_progress_and_the_reporting_phase() {
    let (pulse, log) = recording(|_| None);
    library(&pulse, 4).unwrap();
    let log = log.lock().unwrap();
    let fractions: Vec<f64> = log.iter().map(|s| s.fraction).collect();
    // Decode is a quarter of the job, encode three quarters; each finish is
    // reported too, without moving the fraction.
    assert_eq!(
        fractions,
        [
            0.0625, 0.125, 0.1875, 0.25, 0.25, 0.4375, 0.625, 0.8125, 1.0, 1.0
        ]
    );
    assert_eq!(
        log[1],
        Seen {
            fraction: 0.125,
            phase: "decode".into(),
            units: "rows".into(),
            completed: 2,
            total: Total::Exact(4),
            outcome: None,
        }
    );
    assert_eq!(log[9].phase, "encode");
    assert_eq!(log[9].outcome, Some(Outcome::Succeeded));
    assert_eq!(pulse.fraction(), 1.0);
}

#[test]
fn nested_libraries_share_their_stage_by_weight() {
    let (pulse, log) = recording(|_| None);
    let mut stages = Stages::new(
        &pulse,
        &[
            PhaseSpec::new("outer", 1, Total::Unknown),
            PhaseSpec::new("inner", 1, Total::Unknown),
        ],
    )
    .unwrap();
    stages
        .run_stoppable(|stage| {
            stage.step(10)?; // Unknown total: counts, moves nothing yet.
            Ok::<(), StopReason>(())
        })
        .unwrap();
    // The second stage hands itself to a library that plans its own stages.
    stages
        .run_classified(|_: &RunError<StopReason>| true, |stage| library(stage, 2))
        .unwrap();
    stages.finish().unwrap();
    let log = log.lock().unwrap();
    assert_eq!(log[0].fraction, 0.0);
    assert_eq!(log[0].completed, 10);
    // The Unknown stage moves its half when it finishes.
    assert_eq!(log[1].fraction, 0.5);
    // The library's decode is a quarter of the second half.
    assert_eq!(log[2].fraction, 0.5 + 0.125 / 2.0);
    assert_eq!(log.last().unwrap().fraction, 1.0);
    let mut previous = 0.0;
    for seen in log.iter() {
        assert!(seen.fraction >= previous, "progress never goes back");
        previous = seen.fraction;
    }
}

#[test]
fn an_error_from_the_callback_stops_the_work_at_the_next_check() {
    let (pulse, log) = recording(|p| (p.completed == 3).then_some(StopReason::Cancelled));
    assert_eq!(
        library(&pulse, 4),
        Err(RunError::Work(StopReason::Cancelled))
    );
    // The third row was counted, then `step`'s check stopped the loop, and the
    // callback was not called again, not even for the stage's finish.
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 3);
    assert_eq!(pulse.check(), Err(StopReason::Cancelled));
    assert_eq!(pulse.handle().check(), Err(StopReason::Cancelled));
}

#[test]
fn a_library_that_only_checks_can_be_stopped() {
    let checks = Arc::new(AtomicU64::new(0));
    let seen = Arc::clone(&checks);
    let pulse = FnPulse::new(move |progress| {
        assert!(progress.is_none(), "this library never reports");
        if seen.fetch_add(1, Ordering::Relaxed) + 1 == 5 {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    });
    // Checks every row and never reports.
    let work = |pulse: &dyn Pulse| -> Result<(), StopReason> {
        loop {
            pulse.check()?;
        }
    };
    assert_eq!(work(&pulse), Err(StopReason::Cancelled));
    assert_eq!(checks.load(Ordering::Relaxed), 5);
    // The reason stays, without calling the callback again.
    assert_eq!(pulse.check(), Err(StopReason::Cancelled));
    assert_eq!(pulse.handle().check(), Err(StopReason::Cancelled));
    assert_eq!(checks.load(Ordering::Relaxed), 5);
}

#[test]
fn a_step_reports_then_checks() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let pulse = FnPulse::new(move |progress| {
        sink.lock().unwrap().push(progress.map(|p| p.completed));
        Ok(())
    });
    pulse.step(3).unwrap();
    pulse.check().unwrap();
    assert_eq!(*events.lock().unwrap(), [Some(3), None, None]);
}

#[test]
fn the_first_reason_is_kept() {
    let calls = Arc::new(AtomicU64::new(0));
    let count = Arc::clone(&calls);
    let pulse = FnPulse::new(move |_| {
        count.fetch_add(1, Ordering::Relaxed);
        Err(StopReason::TimedOut)
    });
    pulse.advance(1);
    pulse.advance(1);
    assert_eq!(pulse.check(), Err(StopReason::TimedOut));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn paced_work_reaches_the_callback_once_per_interval() {
    let (pulse, log) = recording(|p| (p.completed >= 8).then_some(StopReason::Cancelled));
    let pulse: &dyn Pulse = &pulse;
    assert!(pulse.live().is_some());
    let mut pace = pulse.paced(4);
    let mut stopped_at = None;
    for row in 1..=20_u64 {
        if pace.step(1).is_err() {
            stopped_at = Some(row);
            break;
        }
    }
    assert_eq!(stopped_at, Some(8));
    let completed: Vec<u64> = log.lock().unwrap().iter().map(|s| s.completed).collect();
    assert_eq!(completed, [4, 8]);
}

#[test]
fn totals_move_the_fraction_as_declared() {
    let (pulse, log) = recording(|_| None);
    let [exact, estimated, unknown] = pulse
        .split_array(
            Execution::Sequence,
            [
                PhaseSpec::new("exact", 1, Total::Exact(2)),
                PhaseSpec::new("estimated", 1, Total::Estimated(2)),
                PhaseSpec::new("unknown", 2, Total::Unknown),
            ],
        )
        .unwrap();
    exact.advance(1);
    exact.finish(Outcome::Succeeded).unwrap();
    // An estimate stops moving the fraction at its total.
    estimated.advance(5);
    estimated.finish(Outcome::Succeeded).unwrap();
    unknown.advance(7);
    unknown.finish(Outcome::Skipped).unwrap();
    let fractions: Vec<f64> = log.lock().unwrap().iter().map(|s| s.fraction).collect();
    assert_eq!(fractions, [0.125, 0.25, 0.5, 0.5, 0.5, 1.0]);
}

#[test]
fn failed_phases_keep_the_fraction_where_it_stopped() {
    let (pulse, _) = recording(|_| None);
    let [first, second] = pulse
        .split_array(
            Execution::Sequence,
            [
                PhaseSpec::new("first", 1, Total::Exact(4)),
                PhaseSpec::new("second", 1, Total::Exact(4)),
            ],
        )
        .unwrap();
    first.advance(2);
    first.finish(Outcome::Failed).unwrap();
    second.finish(Outcome::Cancelled).unwrap();
    assert_eq!(pulse.fraction(), 0.25);
}

#[test]
fn it_keeps_the_planning_rules() {
    let (pulse, _) = recording(|_| None);
    assert_eq!(
        pulse.split(Execution::Sequence, &[]).err(),
        Some(PlanError::EmptyOrZeroWeight)
    );
    let parts = [
        PhaseSpec::new("a", 1, Total::Unknown),
        PhaseSpec::new("b", 1, Total::Unknown),
    ];
    let [a, b] = pulse.split_array(Execution::Sequence, parts).unwrap();
    // A phase splits once, and not after counting.
    assert_eq!(
        pulse.split(Execution::Sequence, &parts).err(),
        Some(PlanError::AlreadyInUse)
    );
    b.advance(1);
    assert_eq!(
        b.split(Execution::Sequence, &parts).err(),
        Some(PlanError::AlreadyInUse)
    );
    // A parent cannot succeed after a child failed.
    let [a1, a2] = a.split_array(Execution::Sequence, parts).unwrap();
    a1.finish(Outcome::Succeeded).unwrap();
    a2.finish(Outcome::Failed).unwrap();
    assert_eq!(
        a.finish(Outcome::Succeeded),
        Err(PlanError::UnsuccessfulChildren)
    );
    b.finish(Outcome::Succeeded).unwrap();
}

#[test]
fn a_parent_stays_open_while_a_child_is_unfinished_and_abandons_on_drop() {
    let (pulse, _) = recording(|_| None);
    let part = [PhaseSpec::new("only", 1, Total::Unknown)];
    let [parent, sibling] = pulse
        .split_array(
            Execution::Sequence,
            [
                PhaseSpec::new("parent", 1, Total::Unknown),
                PhaseSpec::new("sibling", 1, Total::Unknown),
            ],
        )
        .unwrap();
    // A child kept alive elsewhere keeps its parent from finishing.
    let [kept] = parent.split_array(Execution::Sequence, part).unwrap();
    std::mem::forget(kept);
    assert_eq!(
        parent.finish(Outcome::Failed),
        Err(PlanError::UnfinishedChildren)
    );
    // A child dropped unfinished is abandoned: its parent cannot succeed.
    let [dropped] = sibling.split_array(Execution::Sequence, part).unwrap();
    drop(dropped);
    assert_eq!(
        sibling.finish(Outcome::Succeeded),
        Err(PlanError::UnsuccessfulChildren)
    );
}

#[test]
fn workers_report_into_one_phase_at_once() {
    let (pulse, log) = recording(|_| None);
    let [shared] = pulse
        .split_array(
            Execution::ForkJoin,
            [PhaseSpec::new("tiles", 1, Total::Exact(4 * 250))],
        )
        .unwrap();
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for _ in 0..250 {
                    shared.step(1).unwrap();
                }
            });
        }
    });
    shared.finish(Outcome::Succeeded).unwrap();
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 1001);
    assert_eq!(log.iter().map(|s| s.completed).max(), Some(1000));
    assert_eq!(pulse.fraction(), 1.0);
}

#[test]
fn handles_report_and_stop_from_static_threads() {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let (pulse, log) = recording(move |_| {
        flag.load(Ordering::Relaxed)
            .then_some(StopReason::Cancelled)
    });
    let [phase] = pulse
        .split_array(
            Execution::Sequence,
            [PhaseSpec::new("spawned", 1, Total::Exact(2))],
        )
        .unwrap();
    let handle = phase.handle();
    std::thread::spawn(move || handle.step(1))
        .join()
        .unwrap()
        .unwrap();
    assert_eq!(log.lock().unwrap().last().unwrap().fraction, 0.5);
    stop.store(true, Ordering::Relaxed);
    let handle = phase.handle();
    let stopped = std::thread::spawn(move || handle.step(1)).join().unwrap();
    assert_eq!(stopped, Err(StopReason::Cancelled));
    assert_eq!(phase.check(), Err(StopReason::Cancelled));
}

#[test]
fn counts_without_a_plan_reach_the_callback_with_no_fraction() {
    let (pulse, log) = recording(|_| None);
    pulse.step(5).unwrap();
    let log = log.lock().unwrap();
    assert_eq!(log[0].fraction, 0.0);
    assert_eq!(log[0].completed, 5);
    assert_eq!(log[0].phase, "");
    assert_eq!(log[0].units, "items");
    assert_eq!(log[0].total, Total::Unknown);
}

//! Libraries built on `Stages` calling each other, checked with a recording
//! `Pulse` written against the public trait alone.
//!
//! Regression: `Stages` once finished the pulse it was given, so a library
//! running inside another library's stage finished that stage first and the
//! outer finish failed (`Plan(Finished)`), replacing the real result.

use how_far::{
    Child, ChildPulse, Execution, Outcome, PhaseSpec, PlanError, ProgressExt, Pulse, PulseHandle,
    Report, RunError, Stages, Stop, StopReason, Total,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

/// Per-phase records: outcome (`None` while running) and completed units.
#[derive(Default)]
struct Log {
    outcomes: Mutex<BTreeMap<String, Option<Outcome>>>,
    counts: Mutex<BTreeMap<String, u64>>,
}

struct Recorder {
    path: String,
    log: Arc<Log>,
    stop: Arc<AtomicBool>,
    finished: AtomicBool,
}

impl Recorder {
    fn root(log: &Arc<Log>, stop: &Arc<AtomicBool>) -> Self {
        Self::at("job".into(), log, stop)
    }
    fn at(path: String, log: &Arc<Log>, stop: &Arc<AtomicBool>) -> Self {
        log.outcomes.lock().unwrap().insert(path.clone(), None);
        Self {
            path,
            log: log.clone(),
            stop: stop.clone(),
            finished: AtomicBool::new(false),
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if !self.finished.load(Ordering::Relaxed) && self.path != "job" {
            self.log
                .outcomes
                .lock()
                .unwrap()
                .insert(self.path.clone(), Some(Outcome::Abandoned));
        }
    }
}

impl Stop for Recorder {
    fn check(&self) -> Result<(), StopReason> {
        if self.stop.load(Ordering::Acquire) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }
}

impl Report for Recorder {
    fn advance(&self, completed: u64) {
        *self
            .log
            .counts
            .lock()
            .unwrap()
            .entry(self.path.clone())
            .or_default() += completed;
    }
}

impl Pulse for Recorder {
    fn split(&self, _: Execution, parts: &[PhaseSpec<'_>]) -> Result<Vec<Child<'_>>, PlanError> {
        PhaseSpec::validate_split(parts)?;
        Ok(parts
            .iter()
            .map(|part| {
                Child::new(Recorder::at(
                    format!("{}/{}", self.path, part.name),
                    &self.log,
                    &self.stop,
                ))
            })
            .collect())
    }
    fn handle(&self) -> PulseHandle {
        PulseHandle::default()
    }
}

impl ChildPulse for Recorder {
    fn finish(self: Box<Self>, outcome: Outcome) -> Result<(), PlanError> {
        let mut outcomes = self.log.outcomes.lock().unwrap();
        let prefix = format!("{}/", self.path);
        if outcomes
            .iter()
            .any(|(path, outcome)| path.starts_with(&prefix) && outcome.is_none())
        {
            return Err(PlanError::UnfinishedChildren);
        }
        outcomes.insert(self.path.clone(), Some(outcome));
        self.finished.store(true, Ordering::Relaxed);
        Ok(())
    }
}

fn outcome(log: &Log, path: &str) -> Option<Outcome> {
    *log.outcomes.lock().unwrap().get(path).expect(path)
}

/// Library B: a well-behaved `Stages` user.
fn resize(pulse: &dyn Pulse, stop_after: Option<u64>) -> Result<(), RunError<StopReason>> {
    let mut stages = Stages::new(
        pulse,
        &[
            PhaseSpec::new("horizontal", 1, Total::Exact(4)),
            PhaseSpec::new("vertical", 1, Total::Exact(4)),
        ],
    )?;
    for _ in 0..2 {
        stages.run_stoppable(|stage| {
            for row in 0..4 {
                if stop_after == Some(row) {
                    return Err(StopReason::Cancelled);
                }
                stage.step(1)?;
            }
            Ok(())
        })?;
    }
    stages.finish()?;
    Ok(())
}

#[derive(Debug, PartialEq)]
enum CodecError {
    Stopped(StopReason),
    Corrupt,
    Plan(PlanError),
}
impl CodecError {
    fn is_stop(&self) -> bool {
        matches!(self, Self::Stopped(_))
    }
}
impl From<RunError<CodecError>> for CodecError {
    fn from(error: RunError<CodecError>) -> Self {
        match error {
            RunError::Work(error) => error,
            RunError::Plan(error) => Self::Plan(error),
            _ => Self::Corrupt,
        }
    }
}
impl From<RunError<StopReason>> for CodecError {
    fn from(error: RunError<StopReason>) -> Self {
        match error {
            RunError::Work(reason) => Self::Stopped(reason),
            RunError::Plan(error) => Self::Plan(error),
            _ => Self::Corrupt,
        }
    }
}

/// Library C: decodes, then calls library B inside its own stage.
fn thumbnail(pulse: &dyn Pulse, corrupt: bool, stop_after: Option<u64>) -> Result<(), CodecError> {
    let mut stages = Stages::new(
        pulse,
        &[
            PhaseSpec::new("decode", 1, Total::Exact(1)),
            PhaseSpec::new("resize", 4, Total::Unknown),
            PhaseSpec::new("encode", 1, Total::Exact(1)),
        ],
    )
    .map_err(CodecError::Plan)?;
    stages.run_classified(CodecError::is_stop, |stage| {
        if corrupt {
            return Err(CodecError::Corrupt);
        }
        stage.step(1).map_err(CodecError::Stopped)
    })?;
    stages.run_classified(CodecError::is_stop, |stage| {
        resize(stage, stop_after).map_err(CodecError::from)
    })?;
    stages.run_classified(CodecError::is_stop, |stage| {
        stage.step(1).map_err(CodecError::Stopped)
    })?;
    stages.finish().map_err(CodecError::Plan)?;
    Ok(())
}

fn recorder() -> (Arc<Log>, Arc<AtomicBool>) {
    (Arc::default(), Arc::default())
}

#[test]
fn a_stages_library_runs_inside_another_libraries_stage() {
    let (log, stop) = recorder();
    let root = Recorder::root(&log, &stop);
    thumbnail(&root, false, None).unwrap();
    for path in [
        "job/decode",
        "job/resize/horizontal",
        "job/resize/vertical",
        "job/resize",
        "job/encode",
    ] {
        assert_eq!(outcome(&log, path), Some(Outcome::Succeeded), "{path}");
    }
    // The application owns the root; neither library finished it.
    assert_eq!(outcome(&log, "job"), None);
    assert_eq!(log.counts.lock().unwrap()["job/resize/vertical"], 4);
}

#[test]
fn a_nested_stop_keeps_its_error_and_marks_each_level() {
    let (log, stop) = recorder();
    let root = Recorder::root(&log, &stop);
    let error = thumbnail(&root, false, Some(2)).unwrap_err();
    assert_eq!(error, CodecError::Stopped(StopReason::Cancelled));
    assert_eq!(
        outcome(&log, "job/resize/horizontal"),
        Some(Outcome::Cancelled)
    );
    assert_eq!(outcome(&log, "job/resize/vertical"), Some(Outcome::Skipped));
    assert_eq!(outcome(&log, "job/resize"), Some(Outcome::Cancelled));
    assert_eq!(outcome(&log, "job/encode"), Some(Outcome::Skipped));
    assert_eq!(log.counts.lock().unwrap()["job/resize/horizontal"], 2);
}

#[test]
fn a_cancellation_request_surfaces_through_both_libraries() {
    let (log, stop) = recorder();
    let root = Recorder::root(&log, &stop);
    stop.store(true, Ordering::Release);
    let error = thumbnail(&root, false, None).unwrap_err();
    assert_eq!(error, CodecError::Stopped(StopReason::Cancelled));
    assert_eq!(outcome(&log, "job/decode"), Some(Outcome::Cancelled));
    assert_eq!(outcome(&log, "job/resize"), Some(Outcome::Skipped));
}

#[test]
fn a_failure_is_not_recorded_as_a_cancellation() {
    let (log, stop) = recorder();
    let root = Recorder::root(&log, &stop);
    assert_eq!(thumbnail(&root, true, None), Err(CodecError::Corrupt));
    assert_eq!(outcome(&log, "job/decode"), Some(Outcome::Failed));
    assert_eq!(outcome(&log, "job/resize"), Some(Outcome::Skipped));
    assert_eq!(outcome(&log, "job/encode"), Some(Outcome::Skipped));
}

#[test]
fn run_nested_classifies_inline_children_and_flattens_errors() {
    for (corrupt, expected) in [(false, Outcome::Cancelled), (true, Outcome::Failed)] {
        let (log, stop) = recorder();
        let root = Recorder::root(&log, &stop);
        let mut stages = Stages::new(
            &root,
            &[
                PhaseSpec::new("tiles", 1, Total::Unknown),
                PhaseSpec::new("pack", 1, Total::Exact(1)),
            ],
        )
        .unwrap();
        let result = stages.run_nested(CodecError::is_stop, |stage| {
            let [left, right] = stage.split_array(
                Execution::ForkJoin,
                [
                    PhaseSpec::new("left", 1, Total::Exact(1)),
                    PhaseSpec::new("right", 1, Total::Exact(1)),
                ],
            )?;
            left.step(1)
                .map_err(|reason| RunError::Work(CodecError::Stopped(reason)))?;
            left.finish(Outcome::Succeeded)?;
            let error = if corrupt {
                CodecError::Corrupt
            } else {
                CodecError::Stopped(StopReason::Cancelled)
            };
            right.finish(Outcome::from_result(&Err::<(), _>(&error), |e| e.is_stop()))?;
            Err::<(), _>(RunError::Work(error))
        });
        assert!(matches!(result, Err(RunError::Work(_))), "flat error");
        assert_eq!(outcome(&log, "job/tiles"), Some(expected));
        assert_eq!(outcome(&log, "job/tiles/left"), Some(Outcome::Succeeded));
        assert_eq!(outcome(&log, "job/pack"), Some(Outcome::Skipped));
    }
}

#[test]
fn a_stage_that_cannot_finish_reports_a_plan_error_and_skips_the_rest() {
    let (log, stop) = recorder();
    let root = Recorder::root(&log, &stop);
    let mut stages = Stages::new(
        &root,
        &[
            PhaseSpec::new("forgetful", 1, Total::Unknown),
            PhaseSpec::new("later", 1, Total::Exact(1)),
        ],
    )
    .unwrap();
    // The closure splits its stage, finishes nothing, and leaks the children.
    let result = stages.run(|stage| {
        let children = stage.split(
            Execution::ForkJoin,
            &[PhaseSpec::new("left behind", 1, Total::Unknown)],
        )?;
        std::mem::forget(children);
        Ok::<(), PlanError>(())
    });
    assert_eq!(result, Err(RunError::Plan(PlanError::UnfinishedChildren)));
    assert_eq!(outcome(&log, "job/later"), Some(Outcome::Skipped));
    assert_eq!(stages.finish(), Err(PlanError::Finished));
}

#[test]
fn a_panicking_stage_is_abandoned_with_every_later_stage() {
    let (log, stop) = recorder();
    let root = Recorder::root(&log, &stop);
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut stages = Stages::new(
            &root,
            &[
                PhaseSpec::new("decode", 1, Total::Exact(1)),
                PhaseSpec::new("encode", 1, Total::Exact(1)),
            ],
        )
        .unwrap();
        let _ = stages.run_stoppable(|_| -> Result<(), StopReason> { panic!("decoder bug") });
    }));
    assert!(panicked.is_err());
    assert_eq!(outcome(&log, "job/decode"), Some(Outcome::Abandoned));
    assert_eq!(outcome(&log, "job/encode"), Some(Outcome::Abandoned));
}

#[test]
fn after_a_caught_panic_the_plan_is_over() {
    let (log, stop) = recorder();
    let root = Recorder::root(&log, &stop);
    let mut stages = Stages::new(
        &root,
        &[
            PhaseSpec::new("first", 1, Total::Exact(1)),
            PhaseSpec::new("second", 1, Total::Exact(1)),
        ],
    )
    .unwrap();
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        stages.run(|_| -> Result<(), StopReason> { panic!("bug in first") })
    }));
    assert!(panicked.is_err());
    assert_eq!(
        stages.run_stoppable(|stage| stage.step(1)),
        Err(RunError::Plan(PlanError::Finished))
    );
    assert_eq!(outcome(&log, "job/first"), Some(Outcome::Abandoned));
    assert_eq!(outcome(&log, "job/second"), Some(Outcome::Skipped));
    assert_eq!(stages.finish(), Err(PlanError::Finished));
}

#[test]
fn dropping_a_child_unfinished_records_abandonment() {
    let (log, stop) = recorder();
    let root = Recorder::root(&log, &stop);
    let [child] = root
        .split_array(
            Execution::Sequence,
            [PhaseSpec::new("dropped", 1, Total::Unknown)],
        )
        .unwrap();
    drop(child);
    assert_eq!(outcome(&log, "job/dropped"), Some(Outcome::Abandoned));
}

//! Libraries using `&dyn Pulse`, recorded by a real `PulseTree`.

use how_far_along::{
    Execution, Outcome, Phase, PhaseSpec, PlanError, ProgressExt, Pulse, PulseTree, Report,
    RunError, Stages, Status, Stop, StopReason, Total, Unstoppable,
};
use std::{num::NonZeroUsize, sync::Barrier};

fn tree() -> PulseTree {
    PulseTree::new(Phase::new("job", Total::Unknown), Unstoppable)
}

fn pool(n: usize) -> Execution {
    Execution::work_pool(NonZeroUsize::new(n).unwrap())
}

/// A library that plans a serial → fork-join → serial structure by hand.
fn nested_operation(pulse: &dyn Pulse) -> Result<(), StopReason> {
    pulse.check()?;
    let [before, middle, after] = pulse
        .split_array(
            Execution::Sequence,
            [
                PhaseSpec::new("before", 35, Total::Exact(1)),
                PhaseSpec::new("middle", 30, Total::Unknown),
                PhaseSpec::new("after", 35, Total::Exact(1)),
            ],
        )
        .unwrap();
    let [quick, slow] = middle
        .split_array(
            Execution::ForkJoin,
            [
                PhaseSpec::new("quick", 1, Total::Exact(1)),
                PhaseSpec::new("slow", 1, Total::Exact(100)),
            ],
        )
        .unwrap();
    before.step(1)?;
    before.finish(Outcome::Succeeded).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            quick.check().unwrap();
            quick.step(1).unwrap();
            quick.finish(Outcome::Succeeded).unwrap();
        });
        scope.spawn(move || {
            slow.check().unwrap();
            slow.step(100).unwrap();
            slow.finish(Outcome::Succeeded).unwrap();
        });
    });
    middle.finish(Outcome::Succeeded).unwrap();
    after.step(1)?;
    after.finish(Outcome::Succeeded).unwrap();
    Ok(())
}

#[test]
fn one_dyn_pulse_carries_nested_parallel_progress() {
    let tree = tree();
    let observer = tree.observer();
    nested_operation(&tree).unwrap();
    // The library never finishes the pulse it was given.
    assert_eq!(observer.snapshot().status, Status::Running);
    tree.finish(Outcome::Succeeded).unwrap();
    let snapshot = observer.snapshot();
    assert_eq!(snapshot.status, Status::Finished(Outcome::Succeeded));
    assert_eq!(snapshot.fraction(), Some(1.0));
    assert_eq!(snapshot.children[1].children[1].completed, 100);
}

#[test]
fn scoped_workers_share_one_stage_in_a_serial_parallel_serial_plan() {
    let tree = tree();
    let observer = tree.observer();
    let [before, middle, after] = tree
        .split_array(
            Execution::Sequence,
            [
                PhaseSpec::new("before", 35, Total::Exact(1)),
                PhaseSpec::new("middle", 30, Total::Exact(11))
                    .units("blocks")
                    .execution(pool(4)),
                PhaseSpec::new("after", 35, Total::Exact(1)),
            ],
        )
        .unwrap();
    before.step(1).unwrap();
    before.finish(Outcome::Succeeded).unwrap();

    let shared: &dyn Pulse = &middle;
    let start = Barrier::new(5);
    let reported = Barrier::new(5);
    let release = Barrier::new(5);
    std::thread::scope(|scope| {
        for count in [1, 2, 3, 5] {
            let (start, reported, release) = (&start, &reported, &release);
            scope.spawn(move || {
                start.wait();
                shared.check().unwrap();
                shared.step(1).unwrap();
                reported.wait();
                release.wait();
                shared.step(count - 1).unwrap();
                shared.check().unwrap();
            });
        }
        start.wait();
        reported.wait();
        let live = observer.snapshot();
        release.wait();
        assert_eq!(
            live.children[0].status,
            Status::Finished(Outcome::Succeeded)
        );
        assert_eq!(live.children[1].completed, 4);
        assert_eq!(live.children[1].status, Status::Running);
        assert_eq!(live.children[1].total, Total::Exact(11));
        assert_eq!(live.children[1].execution, pool(4));
        assert!(live.children[1].children.is_empty());
        assert_eq!(live.children[2].status, Status::Pending);
    });
    assert_eq!(observer.snapshot().children[1].completed, 11);
    middle.finish(Outcome::Succeeded).unwrap();
    after.step(1).unwrap();
    after.finish(Outcome::Succeeded).unwrap();
    tree.finish(Outcome::Succeeded).unwrap();
    let final_state = observer.snapshot();
    assert_eq!(final_state.status, Status::Finished(Outcome::Succeeded));
    assert_eq!(final_state.fraction(), Some(1.0));
}

#[test]
fn rayon_chunks_share_one_stage_without_a_child_per_worker() {
    use rayon::prelude::*;

    let tree = PulseTree::new(Phase::new("encode", Total::Unknown), Unstoppable);
    let observer = tree.observer();
    let mut stages = Stages::new(
        &tree,
        &[
            PhaseSpec::new("prepare", 35, Total::Exact(1)),
            PhaseSpec::new("encode chunks", 30, Total::Exact(16)).execution(pool(4)),
            PhaseSpec::new("write", 35, Total::Exact(1)),
        ],
    )
    .unwrap();
    stages.run_stoppable(|stage| stage.step(1)).unwrap();
    let workers = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap();
    stages
        .run_stoppable(|stage| {
            workers.install(|| {
                [1_u64, 4, 2, 9].par_iter().try_for_each(|&units| {
                    stage.check()?;
                    stage.step(units)
                })
            })
        })
        .unwrap();
    stages.run_stoppable(|stage| stage.step(1)).unwrap();
    stages.finish().unwrap();
    tree.finish(Outcome::Succeeded).unwrap();
    let final_state = observer.snapshot();
    assert_eq!(final_state.children[1].completed, 16);
    assert_eq!(final_state.fraction(), Some(1.0));
}

#[test]
fn the_first_report_ends_planning_and_a_dropped_child_is_abandoned() {
    let tree = PulseTree::new(Phase::new("job", Total::Exact(2)), Unstoppable);
    tree.step(1).unwrap();
    assert_eq!(
        tree.split(
            Execution::Sequence,
            &[PhaseSpec::new("late", 1, Total::Exact(1))]
        )
        .err(),
        Some(PlanError::AlreadyInUse)
    );
    tree.finish(Outcome::Succeeded).unwrap();

    let tree = tree_named("job");
    let observer = tree.observer();
    let children = tree
        .split(
            Execution::Sequence,
            &[PhaseSpec::new("dropped", 1, Total::Exact(1))],
        )
        .unwrap();
    drop(children);
    assert_eq!(
        observer.snapshot().children[0].status,
        Status::Finished(Outcome::Abandoned)
    );
    assert_eq!(
        tree.finish(Outcome::Succeeded),
        Err(PlanError::UnsuccessfulChildren)
    );
    // A failed finish leaves no outcome to report, so the root is abandoned.
    assert_eq!(
        observer.snapshot().status,
        Status::Finished(Outcome::Abandoned)
    );
}

fn tree_named(name: &str) -> PulseTree {
    PulseTree::new(Phase::new(name, Total::Unknown), Unstoppable)
}

#[test]
fn stages_finish_each_stage_and_classify_stops_apart_from_failures() {
    let specs = [
        PhaseSpec::new("before", 1, Total::Exact(1)),
        PhaseSpec::new("current", 1, Total::Exact(2)),
        PhaseSpec::new("after", 1, Total::Exact(1)),
    ];
    for (stoppable, outcome) in [(true, Outcome::Cancelled), (false, Outcome::Failed)] {
        let tree = tree();
        let observer = tree.observer();
        let mut stages = Stages::new(&tree, &specs).unwrap();
        stages.run(|stage| stage.step(1)).unwrap();
        let result = if stoppable {
            stages.run_stoppable(|stage| {
                stage.step(1)?;
                Err::<(), _>(StopReason::Cancelled)
            })
        } else {
            stages.run(|stage| {
                stage.step(1)?;
                Err::<(), _>(StopReason::Cancelled)
            })
        };
        assert_eq!(result, Err(RunError::Work(StopReason::Cancelled)));
        assert_eq!(stages.finish(), Err(PlanError::Finished));
        let snapshot = observer.snapshot();
        assert_eq!(
            snapshot.status,
            Status::Running,
            "the app finishes the root"
        );
        assert_eq!(
            snapshot.children[0].status,
            Status::Finished(Outcome::Succeeded)
        );
        assert_eq!(snapshot.children[1].completed, 1);
        assert_eq!(snapshot.children[1].status, Status::Finished(outcome));
        assert_eq!(
            snapshot.children[2].status,
            Status::Finished(Outcome::Skipped)
        );
        tree.finish(outcome).unwrap();
        assert_eq!(observer.snapshot().status, Status::Finished(outcome));
    }
}

#[test]
fn a_classified_stage_keeps_codec_failure_apart_from_cancellation() {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum WorkError {
        Stopped,
        InvalidFrame,
    }

    for (error, expected) in [
        (WorkError::Stopped, Outcome::Cancelled),
        (WorkError::InvalidFrame, Outcome::Failed),
    ] {
        let tree = PulseTree::new(Phase::new("encode", Total::Unknown), Unstoppable);
        let observer = tree.observer();
        let mut stages = Stages::new(
            &tree,
            &[
                PhaseSpec::new("frames", 9, Total::Exact(2)),
                PhaseSpec::new("flush", 1, Total::Exact(1)),
            ],
        )
        .unwrap();
        let result = stages.run_classified(
            |error| matches!(error, WorkError::Stopped),
            |stage| {
                stage.step(1).unwrap();
                Err::<(), _>(error)
            },
        );
        assert_eq!(result, Err(RunError::Work(error)));
        drop(stages); // A library's stages end when it returns.
        tree.finish(Outcome::from_result(&result, |e| {
            matches!(e, RunError::Work(WorkError::Stopped))
        }))
        .unwrap();
        let snapshot = observer.snapshot();
        assert_eq!(snapshot.status, Status::Finished(expected));
        assert_eq!(snapshot.children[0].completed, 1);
        assert_eq!(snapshot.children[0].status, Status::Finished(expected));
        assert_eq!(
            snapshot.children[1].status,
            Status::Finished(Outcome::Skipped)
        );
    }
}

#[test]
fn unrun_stages_cannot_let_the_root_succeed() {
    let tree = tree();
    let observer = tree.observer();
    let stages = Stages::new(&tree, &[PhaseSpec::new("pending", 1, Total::Exact(1))]).unwrap();
    assert_eq!(stages.finish(), Err(PlanError::UnfinishedChildren));
    assert_eq!(
        observer.snapshot().children[0].status,
        Status::Finished(Outcome::Abandoned)
    );
    assert_eq!(
        tree.finish(Outcome::Succeeded),
        Err(PlanError::UnsuccessfulChildren)
    );
}

#[test]
fn run_nested_wraps_an_inline_parallel_stage() {
    let tree = tree();
    let observer = tree.observer();
    let mut stages = Stages::new(
        &tree,
        &[
            PhaseSpec::new("before", 35, Total::Exact(1)),
            PhaseSpec::new("parallel", 30, Total::Unknown),
            PhaseSpec::new("after", 35, Total::Exact(1)),
        ],
    )
    .unwrap();
    stages.run(|stage| stage.step(1)).unwrap();
    stages
        .run_nested(
            |_: &StopReason| true,
            |middle| {
                let children = middle.split(
                    Execution::ForkJoin,
                    &[
                        PhaseSpec::new("quick", 1, Total::Exact(1)),
                        PhaseSpec::new("slow", 1, Total::Exact(100)),
                    ],
                )?;
                std::thread::scope(|scope| {
                    for (child, count) in children.into_iter().zip([1, 100]) {
                        scope.spawn(move || {
                            child.check().unwrap();
                            child.step(count).unwrap();
                            child.finish(Outcome::Succeeded).unwrap();
                        });
                    }
                });
                Ok(())
            },
        )
        .unwrap();
    stages.run(|stage| stage.step(1)).unwrap();
    stages.finish().unwrap();
    tree.finish(Outcome::Succeeded).unwrap();
    let snapshot = observer.snapshot();
    assert_eq!(snapshot.status, Status::Finished(Outcome::Succeeded));
    assert_eq!(snapshot.children[1].weight, 30);
    assert_eq!(snapshot.children[1].children[1].completed, 100);
}

#[test]
fn a_nested_plan_error_fails_the_stage_and_skips_the_rest() {
    let tree = tree();
    let observer = tree.observer();
    let mut stages = Stages::new(
        &tree,
        &[
            PhaseSpec::new("invalid branch", 1, Total::Unknown),
            PhaseSpec::new("later", 1, Total::Exact(1)),
        ],
    )
    .unwrap();
    let result = stages.run_nested(
        |_: &StopReason| true,
        |middle| {
            middle.split(Execution::ForkJoin, &[])?;
            Ok(())
        },
    );
    assert_eq!(result, Err(RunError::Plan(PlanError::EmptyOrZeroWeight)));
    let snapshot = observer.snapshot();
    assert_eq!(
        snapshot.children[0].status,
        Status::Finished(Outcome::Failed)
    );
    assert_eq!(
        snapshot.children[1].status,
        Status::Finished(Outcome::Skipped)
    );
}

/// Library B: two passes over rows, with its own stages.
fn resize(pulse: &dyn Pulse, rows: u64) -> Result<(), RunError<StopReason>> {
    let mut stages = Stages::new(
        pulse,
        &[
            PhaseSpec::new("horizontal", 1, Total::Exact(rows)),
            PhaseSpec::new("vertical", 1, Total::Exact(rows)),
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

/// Library A: decodes, then calls library B inside one of its own stages.
fn thumbnail(pulse: &dyn Pulse) -> Result<(), RunError<StopReason>> {
    let mut stages = Stages::new(
        pulse,
        &[
            PhaseSpec::new("decode", 1, Total::Exact(1)),
            PhaseSpec::new("resize", 4, Total::Unknown),
        ],
    )?;
    stages.run_stoppable(|stage| stage.step(1))?;
    stages.run_nested(|_| true, |stage| resize(stage, 8))?;
    stages.finish()?;
    Ok(())
}

#[test]
fn a_stages_library_runs_inside_another_libraries_stage() {
    let tree = tree();
    let observer = tree.observer();
    let result = thumbnail(&tree);
    assert_eq!(result, Ok(()));
    tree.finish(Outcome::Succeeded).unwrap();
    let snapshot = observer.snapshot();
    assert_eq!(snapshot.status, Status::Finished(Outcome::Succeeded));
    let resize = &snapshot.children[1];
    assert_eq!(resize.status, Status::Finished(Outcome::Succeeded));
    assert_eq!(resize.children[1].name, "vertical");
    assert_eq!(resize.children[1].completed, 8);
    assert_eq!(snapshot.fraction(), Some(1.0));
}

#[test]
fn cancelling_a_nested_library_records_every_level_and_keeps_the_error() {
    let stop = almost_enough::Stopper::new();
    let tree = PulseTree::new(Phase::new("job", Total::Unknown), stop.clone());
    let observer = tree.observer();
    let mut stages = Stages::new(
        &tree,
        &[
            PhaseSpec::new("decode", 1, Total::Exact(1)),
            PhaseSpec::new("resize", 4, Total::Unknown),
            PhaseSpec::new("write", 1, Total::Exact(1)),
        ],
    )
    .unwrap();
    stages.run_stoppable(|stage| stage.step(1)).unwrap();
    stop.cancel();
    let result = stages.run_nested(|_| true, |stage| resize(stage, 8));
    assert_eq!(result, Err(RunError::Work(StopReason::Cancelled)));
    drop(stages);
    tree.finish(Outcome::Cancelled).unwrap();
    let snapshot = observer.snapshot();
    assert_eq!(snapshot.status, Status::Finished(Outcome::Cancelled));
    assert_eq!(
        snapshot.children[1].status,
        Status::Finished(Outcome::Cancelled)
    );
    assert_eq!(
        snapshot.children[1].children[0].status,
        Status::Finished(Outcome::Cancelled)
    );
    assert_eq!(
        snapshot.children[1].children[1].status,
        Status::Finished(Outcome::Skipped)
    );
    assert_eq!(
        snapshot.children[2].status,
        Status::Finished(Outcome::Skipped)
    );
}

#[test]
fn a_count_only_library_leaves_finishing_to_the_application() {
    fn count_only(pulse: &dyn Pulse) -> Result<(), StopReason> {
        for _ in 0..3 {
            pulse.step(1)?;
        }
        Ok(())
    }
    let tree = PulseTree::new(Phase::new("job", Total::Exact(3)), Unstoppable);
    let observer = tree.observer();
    let result = count_only(&tree);
    tree.finish(Outcome::from_result(&result, |_| true))
        .unwrap();
    let snapshot = observer.snapshot();
    assert_eq!(snapshot.status, Status::Finished(Outcome::Succeeded));
    assert_eq!(snapshot.completed, 3);

    // Without a finish, the record says nobody published an outcome.
    let tree = PulseTree::new(Phase::new("job", Total::Exact(3)), Unstoppable);
    let observer = tree.observer();
    count_only(&tree).unwrap();
    drop(tree);
    assert_eq!(
        observer.snapshot().status,
        Status::Finished(Outcome::Abandoned)
    );
}

#[test]
fn a_tree_shares_its_stop_policy_with_every_phase_and_handle() {
    let stop = almost_enough::Stopper::new();
    let tree = PulseTree::new(Phase::new("job", Total::Unknown), stop.clone());
    let [child] = tree
        .split_array(
            Execution::Sequence,
            [PhaseSpec::new("child", 1, Total::Exact(1))],
        )
        .unwrap();
    let handle = child.handle();
    assert!(tree.check().is_ok() && child.check().is_ok() && handle.check().is_ok());
    stop.cancel();
    for result in [
        tree.check(),
        child.check(),
        handle.check(),
        handle.stop.check(),
    ] {
        assert_eq!(result, Err(StopReason::Cancelled));
    }
}

#[test]
fn gating_a_tree_follows_what_it_can_still_do() {
    // A never-stopping leaf still counts, so it stays.
    let tree = PulseTree::new(Phase::new("job", Total::Unknown), Unstoppable);
    assert!(tree.gated().is_some());
    // A failed split leaves it a counting leaf.
    assert!(tree.split(Execution::Sequence, &[]).is_err());
    assert!(tree.may_report() && tree.gated().is_some());
    // Once split, a never-stopping branch can do nothing: gate it away.
    let [child] = tree
        .split_array(
            Execution::Sequence,
            [PhaseSpec::new("child", 1, Total::Exact(1))],
        )
        .unwrap();
    assert!(!tree.may_report());
    assert!(tree.gated().is_none());
    assert!(child.gated().is_some());
    child.gated().step(1).unwrap();
    child.finish(Outcome::Succeeded).unwrap();
    tree.finish(Outcome::Succeeded).unwrap();

    // A branch that may still be cancelled keeps its stop checks.
    let stoppable = PulseTree::new(
        Phase::new("job", Total::Unknown),
        almost_enough::Stopper::new(),
    );
    let children = stoppable
        .split(
            Execution::Sequence,
            &[PhaseSpec::new("child", 1, Total::Exact(1))],
        )
        .unwrap();
    assert!(stoppable.gated().is_some());
    drop(children);
}

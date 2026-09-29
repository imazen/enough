use enough::{Stop, StopReason, Unstoppable};
use howfar::{Execution, Outcome, PhaseSpec, Pulse, Report, RunError, Steps, Total};
use howfar_along::{Phase, PlanError, PulseTree, Status};

fn nested_operation(pulse: &dyn Pulse) -> Result<(), StopReason> {
    pulse.check()?;
    let [before, middle, after] = pulse
        .split(
            Execution::Sequence,
            &[
                PhaseSpec::new("before", 35, Total::Exact(1)),
                PhaseSpec::new("middle", 30, Total::Unknown),
                PhaseSpec::new("after", 35, Total::Exact(1)),
            ],
        )
        .unwrap()
        .try_into()
        .unwrap_or_else(|_| panic!("three children"));
    let [quick, slow] = middle
        .split(
            Execution::ForkJoin,
            &[
                PhaseSpec::new("quick", 1, Total::Exact(1)),
                PhaseSpec::new("slow", 1, Total::Exact(100)),
            ],
        )
        .unwrap()
        .try_into()
        .unwrap_or_else(|_| panic!("two children"));
    before.advance(1);
    before.finish(Outcome::Succeeded).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            quick.check().unwrap();
            quick.advance(1);
            quick.finish(Outcome::Succeeded).unwrap();
        });
        scope.spawn(|| {
            slow.check().unwrap();
            slow.advance(100);
            slow.finish(Outcome::Succeeded).unwrap();
        });
    });
    middle.finish(Outcome::Succeeded).unwrap();
    after.advance(1);
    after.finish(Outcome::Succeeded).unwrap();
    pulse.finish(Outcome::Succeeded).unwrap();
    Ok(())
}

#[test]
fn one_dyn_pulse_carries_nested_parallel_progress_and_cancellation() {
    let phase = Phase::new("job", Total::Unknown);
    let pulse = PulseTree::new(phase, &Unstoppable);
    let observer = pulse.observer();
    nested_operation(&pulse).unwrap();
    let snapshot = observer.snapshot();
    assert_eq!(snapshot.status, Status::Finished(Outcome::Succeeded));
    assert_eq!(snapshot.fraction(), Some(1.0));
    assert_eq!(snapshot.children[1].children[1].completed, 100);
}

#[test]
fn first_report_prevents_late_replanning_and_abandoned_child_freezes() {
    let phase = Phase::new("job", Total::Exact(2));
    let pulse = PulseTree::new(phase, &Unstoppable);
    pulse.advance(1);
    assert!(matches!(
        pulse.split(
            Execution::Sequence,
            &[PhaseSpec::new("late", 1, Total::Exact(1))]
        ),
        Err(PlanError::AlreadyInUse)
    ));
    pulse.finish(Outcome::Succeeded).unwrap();

    let phase = Phase::new("job", Total::Unknown);
    let pulse = PulseTree::new(phase, &Unstoppable);
    let observer = pulse.observer();
    let children = pulse
        .split(
            Execution::Sequence,
            &[PhaseSpec::new("dropped", 1, Total::Exact(1))],
        )
        .unwrap();
    drop(children);
    assert_eq!(
        pulse.finish(Outcome::Succeeded),
        Err(PlanError::UnsuccessfulChildren)
    );
    assert_eq!(
        observer.snapshot().children[0].status,
        Status::Finished(Outcome::Abandoned)
    );
}

#[test]
fn steps_finish_success_and_classify_stop_or_work_failure() {
    let specs = [
        PhaseSpec::new("before", 1, Total::Exact(1)),
        PhaseSpec::new("current", 1, Total::Exact(2)),
        PhaseSpec::new("after", 1, Total::Exact(1)),
    ];
    for (stoppable, failure) in [(true, Outcome::Cancelled), (false, Outcome::Failed)] {
        let pulse = PulseTree::new(Phase::new("job", Total::Unknown), &Unstoppable);
        let observer = pulse.observer();
        let mut steps = Steps::new(&pulse, &specs).unwrap();
        steps
            .run(|stage| {
                stage.advance(1);
                Ok::<(), StopReason>(())
            })
            .unwrap();
        let result = if stoppable {
            steps.run_stoppable(|stage| {
                stage.advance(1);
                Err::<(), _>(StopReason::Cancelled)
            })
        } else {
            steps.run(|stage| {
                stage.advance(1);
                Err::<(), _>(StopReason::Cancelled)
            })
        };
        assert!(matches!(result, Err(RunError::Work(StopReason::Cancelled))));
        assert_eq!(steps.finish(), Err(PlanError::Finished));
        let snapshot = observer.snapshot();
        assert_eq!(snapshot.status, Status::Finished(failure));
        assert_eq!(
            snapshot.children[0].status,
            Status::Finished(Outcome::Succeeded)
        );
        assert_eq!(snapshot.children[1].completed, 1);
        assert_eq!(snapshot.children[1].status, Status::Finished(failure));
        assert_eq!(
            snapshot.children[2].status,
            Status::Finished(Outcome::Skipped)
        );
    }
}

#[test]
fn unfinished_steps_cannot_mark_a_parent_successful() {
    let pulse = PulseTree::new(Phase::new("job", Total::Unknown), &Unstoppable);
    let observer = pulse.observer();
    let steps = Steps::new(&pulse, &[PhaseSpec::new("pending", 1, Total::Exact(1))]).unwrap();
    assert_eq!(steps.finish(), Err(PlanError::UnfinishedChildren));
    assert_eq!(
        observer.snapshot().children[0].status,
        Status::Finished(Outcome::Abandoned)
    );
}

#[test]
fn steps_can_wrap_a_parallel_middle_stage_without_nested_errors() {
    let pulse = PulseTree::new(Phase::new("job", Total::Unknown), &Unstoppable);
    let observer = pulse.observer();
    let mut steps = Steps::new(
        &pulse,
        &[
            PhaseSpec::new("before", 35, Total::Exact(1)),
            PhaseSpec::new("parallel", 30, Total::Unknown),
            PhaseSpec::new("after", 35, Total::Exact(1)),
        ],
    )
    .unwrap();
    steps
        .run(|stage| {
            stage.advance(1);
            Ok::<(), StopReason>(())
        })
        .unwrap();
    steps
        .run_nested_stoppable(|middle| {
            let children = middle.split(
                Execution::ForkJoin,
                &[
                    PhaseSpec::new("quick", 1, Total::Exact(1)),
                    PhaseSpec::new("slow", 1, Total::Exact(100)),
                ],
            )?;
            std::thread::scope(|scope| {
                for (child, count) in children.iter().zip([1, 100]) {
                    scope.spawn(move || {
                        child.check().unwrap();
                        child.advance(count);
                        child.finish(Outcome::Succeeded).unwrap();
                    });
                }
            });
            Ok::<(), RunError<StopReason>>(())
        })
        .unwrap();
    steps
        .run(|stage| {
            stage.advance(1);
            Ok::<(), StopReason>(())
        })
        .unwrap();
    steps.finish().unwrap();
    let snapshot = observer.snapshot();
    assert_eq!(snapshot.status, Status::Finished(Outcome::Succeeded));
    assert_eq!(snapshot.children[1].weight, 30);
    assert_eq!(snapshot.children[1].children[1].completed, 100);
}

#[test]
fn a_nested_plan_error_fails_the_active_stage_and_skips_the_tail() {
    let pulse = PulseTree::new(Phase::new("job", Total::Unknown), &Unstoppable);
    let observer = pulse.observer();
    let mut steps = Steps::new(
        &pulse,
        &[
            PhaseSpec::new("invalid branch", 1, Total::Unknown),
            PhaseSpec::new("later", 1, Total::Exact(1)),
        ],
    )
    .unwrap();
    let result = steps.run_nested_stoppable::<(), StopReason>(|middle| {
        middle.split(Execution::ForkJoin, &[])?;
        Ok(())
    });
    assert!(matches!(
        result,
        Err(RunError::Plan(PlanError::EmptyOrZeroWeight))
    ));
    let snapshot = observer.snapshot();
    assert_eq!(snapshot.status, Status::Finished(Outcome::Failed));
    assert_eq!(
        snapshot.children[0].status,
        Status::Finished(Outcome::Failed)
    );
    assert_eq!(
        snapshot.children[1].status,
        Status::Finished(Outcome::Skipped)
    );
}

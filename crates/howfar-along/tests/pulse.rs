use enough::{Stop, StopReason, Unstoppable};
use howfar::{Execution, Outcome, PhaseSpec, Pulse, Report, Total};
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

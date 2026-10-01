//! The `Pulse` contract with the no-op implementation.

use how_far::{
    Execution, NoPulse, Outcome, PhaseSpec, PlanError, ProgressExt, Pulse, Report, RunError,
    Stages, Stop, StopReason, Total,
};
use std::num::NonZeroUsize;

fn library_operation(pulse: &dyn Pulse) -> Result<(), StopReason> {
    let [phase] = pulse
        .split_array(
            Execution::Sequence,
            [PhaseSpec::new("rows", 1, Total::Exact(2)).units("rows")],
        )
        .unwrap();
    phase.check()?;
    phase.step(2)?;
    phase.finish(Outcome::Succeeded).unwrap();
    Ok(())
}

#[test]
fn no_pulse_supports_the_whole_nested_api() {
    library_operation(&NoPulse).unwrap();
    assert!(!NoPulse.may_stop());
    assert!(!NoPulse.may_report());
    let handle = NoPulse.handle();
    assert!(handle.stop.is_none() && handle.report.is_none());
    handle.step(1).unwrap();
}

#[test]
fn no_pulse_still_validates_plans() {
    assert_eq!(
        NoPulse.split(Execution::Sequence, &[]).err(),
        Some(PlanError::EmptyOrZeroWeight)
    );
    assert_eq!(
        NoPulse
            .split(
                Execution::Sequence,
                &[PhaseSpec::new("zero", 0, Total::Unknown)]
            )
            .err(),
        Some(PlanError::EmptyOrZeroWeight)
    );
    assert_eq!(
        NoPulse
            .split(
                Execution::Sequence,
                &[
                    PhaseSpec::new("a", u64::MAX, Total::Unknown),
                    PhaseSpec::new("b", 1, Total::Unknown),
                ]
            )
            .err(),
        Some(PlanError::Overflow)
    );
}

#[test]
fn a_pulse_reference_serves_existing_stop_and_report_seams() {
    fn old_stop_site(stop: &dyn Stop) -> Result<(), StopReason> {
        stop.check()
    }
    fn old_report_site(report: &dyn Report) {
        report.advance(1);
    }
    fn generic_site(work: &(impl Stop + Report + ?Sized)) -> Result<(), StopReason> {
        work.step(1)
    }
    let pulse: &dyn Pulse = &NoPulse;
    // `&dyn Pulse` is itself `Stop + Report`, so this needs no trait upcasting.
    old_stop_site(&pulse).unwrap();
    old_report_site(&pulse);
    generic_site(pulse).unwrap();
}

#[test]
fn stages_run_each_declared_stage_once() {
    let pulse: &dyn Pulse = &NoPulse;
    let mut stages = Stages::new(
        pulse,
        &[
            PhaseSpec::new("decode", 2, Total::Exact(3)),
            PhaseSpec::new("encode", 1, Total::Exact(1)),
        ],
    )
    .unwrap();
    stages
        .run_stoppable(|stage| {
            stage.check()?;
            stage.step(3)
        })
        .unwrap();
    stages.run(|stage| stage.step(1)).unwrap();
    stages.finish().unwrap();

    let mut short = Stages::new(pulse, &[PhaseSpec::new("only", 1, Total::Unknown)]).unwrap();
    assert!(matches!(short.run(|_| Ok::<(), ()>(())), Ok(())));
    assert!(matches!(
        short.run(|_| Ok::<(), ()>(())),
        Err(RunError::Plan(PlanError::NoMoreStages))
    ));

    let unrun = Stages::new(pulse, &[PhaseSpec::new("never", 1, Total::Unknown)]).unwrap();
    assert_eq!(unrun.finish(), Err(PlanError::UnfinishedChildren));
}

#[test]
fn work_pool_parallelism_cannot_be_zero() {
    let pool = Execution::work_pool(NonZeroUsize::new(4).unwrap());
    assert!(matches!(
        pool,
        Execution::WorkPool { max_parallelism, .. } if max_parallelism.get() == 4
    ));
    let spec = PhaseSpec::new("blocks", 1, Total::Exact(64))
        .units("blocks")
        .execution(pool);
    assert_eq!(spec.execution, pool);
}

#[test]
fn outcomes_follow_results() {
    let ok: Result<u8, StopReason> = Ok(1);
    let stopped: Result<u8, StopReason> = Err(StopReason::TimedOut);
    let failed: Result<u8, &str> = Err("corrupt input");
    assert_eq!(Outcome::from_result(&ok, |_| true), Outcome::Succeeded);
    assert_eq!(Outcome::from_result(&stopped, |_| true), Outcome::Cancelled);
    assert_eq!(Outcome::from_result(&failed, |_| false), Outcome::Failed);
}

#[test]
#[should_panic(expected = "returned 0 children for 1 parts")]
fn split_array_rejects_a_pulse_that_breaks_the_split_contract() {
    struct Broken;
    impl Stop for Broken {
        fn check(&self) -> Result<(), StopReason> {
            Ok(())
        }
    }
    impl Report for Broken {
        fn advance(&self, _: u64) {}
    }
    impl Pulse for Broken {
        fn split(
            &self,
            _: Execution,
            _: &[PhaseSpec<'_>],
        ) -> Result<Vec<how_far::Child<'_>>, PlanError> {
            Ok(Vec::new())
        }
        fn handle(&self) -> how_far::PulseHandle {
            how_far::PulseHandle::default()
        }
    }
    let _ = Broken.split_array(
        Execution::Sequence,
        [PhaseSpec::new("one", 1, Total::Unknown)],
    );
}

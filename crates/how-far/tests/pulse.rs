#![cfg(feature = "checked")]
//! The `Pulse` contract with the no-op implementation.

use how_far::{
    Execution, NoPulse, Outcome, PhaseSpec, PlanError, ProgressExt, Pulse, Report, RunError, Stop,
    StopReason, Total, TryStages,
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
fn implementations_validate_splits_with_the_shared_check() {
    let parts = [
        PhaseSpec::new("a", 1, Total::Unknown),
        PhaseSpec::new("b", 3, Total::Unknown),
    ];
    assert_eq!(PhaseSpec::validate_split(&parts), Ok(4));
    assert_eq!(
        PhaseSpec::validate_split(&[]),
        Err(PlanError::EmptyOrZeroWeight)
    );
    assert_eq!(
        PhaseSpec::validate_split(&[
            PhaseSpec::new("a", u64::MAX, Total::Unknown),
            PhaseSpec::new("b", 1, Total::Unknown),
        ]),
        Err(PlanError::Overflow)
    );
}

#[test]
fn an_inert_child_neither_stops_nor_reports() {
    let child = how_far::Child::inert();
    assert!(!child.may_stop() && !child.may_report());
    assert!(child.live().is_none());
    child.step(3).unwrap();
    child.finish(Outcome::Failed).unwrap();
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
    // Trait upcasting passes the pulse itself; a reference to it works too.
    old_stop_site(pulse).unwrap();
    old_report_site(pulse);
    old_stop_site(&pulse).unwrap();
    old_report_site(&pulse);
    generic_site(pulse).unwrap();
}

#[test]
fn stages_run_each_declared_stage_once() {
    let pulse: &dyn Pulse = &NoPulse;
    let mut stages = TryStages::new(
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

    let mut short = TryStages::new(pulse, &[PhaseSpec::new("only", 1, Total::Unknown)]).unwrap();
    assert!(matches!(short.run(|_| Ok::<(), ()>(())), Ok(())));
    assert!(matches!(
        short.run(|_| Ok::<(), ()>(())),
        Err(RunError::Plan(PlanError::NoMoreStages))
    ));

    let unrun = TryStages::new(pulse, &[PhaseSpec::new("never", 1, Total::Unknown)]).unwrap();
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

#[test]
fn gating_drops_only_pulses_that_can_neither_stop_nor_report() {
    use how_far::{NoReport, ProgressWithStop, Unstoppable};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Count(AtomicU64);
    impl Report for Count {
        fn advance(&self, n: u64) {
            self.0.fetch_add(n, Ordering::Relaxed);
        }
    }
    struct Stopping;
    impl Stop for Stopping {
        fn check(&self) -> Result<(), StopReason> {
            Err(StopReason::Cancelled)
        }
    }

    let inert: &dyn Pulse = &NoPulse;
    assert!(inert.live().is_none());
    assert!(NoPulse.live().is_none());
    assert!(
        ProgressWithStop::new(Unstoppable, NoReport)
            .live()
            .is_none()
    );
    let [child] = NoPulse
        .split_array(
            Execution::Sequence,
            [PhaseSpec::new("child", 1, Total::Unknown)],
        )
        .unwrap();
    assert!(child.live().is_none());
    // `None` checks and steps like the original, without a call.
    let live = inert.live();
    live.check().unwrap();
    live.advance(5);
    live.step(5).unwrap();
    assert!(!live.may_stop() && !live.may_report());

    let count = Count(AtomicU64::new(0));
    let counting = ProgressWithStop::new(Unstoppable, &count);
    let live = counting.live();
    assert!(live.is_some());
    live.step(3).unwrap();
    live.advance(2);
    assert_eq!(count.0.load(Ordering::Relaxed), 5);

    let stopping = ProgressWithStop::new(Stopping, NoReport);
    assert_eq!(stopping.live().check(), Err(StopReason::Cancelled));
}

#[test]
fn the_prelude_brings_every_checkpoint_method_into_scope() {
    mod library {
        use how_far::prelude::*;

        pub fn run(pulse: &dyn Pulse) -> Result<(), how_far::StopReason> {
            let live = pulse.live();
            live.check()?;
            live.advance(1);
            live.step(1)?;
            let handle = pulse.handle();
            handle.check()?;
            handle.advance(1);
            handle.step(1)
        }
    }
    library::run(&NoPulse).unwrap();
}

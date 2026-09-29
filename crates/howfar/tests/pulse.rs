use enough::{Stop, StopReason};
use howfar::{
    Execution, NoPulse, Outcome, PhaseSpec, PlanError, Pulse, Report, RunError, Steps, Total,
};

fn library_operation(pulse: &dyn Pulse) -> Result<(), StopReason> {
    let [phase] = pulse
        .split(
            Execution::Sequence,
            &[PhaseSpec::new("rows", 1, Total::Exact(2)).units("rows")],
        )
        .unwrap()
        .try_into()
        .unwrap_or_else(|_| panic!("one child"));
    phase.check()?;
    phase.advance(2);
    phase.finish(Outcome::Succeeded).unwrap();
    pulse.finish(Outcome::Succeeded).unwrap();
    Ok(())
}

#[test]
fn no_pulse_supports_the_same_nested_dyn_api() {
    library_operation(&NoPulse).unwrap();
    assert!(!NoPulse.may_stop());
    assert!(!NoPulse.may_report());
    assert_eq!(
        NoPulse.split(Execution::Sequence, &[]).err(),
        Some(PlanError::EmptyOrZeroWeight)
    );
}

#[test]
fn a_dyn_pulse_upcasts_to_existing_stop_and_report_sites() {
    fn old_stop_site(stop: &dyn Stop) -> Result<(), StopReason> {
        stop.check()
    }
    fn old_report_site(report: &dyn Report) {
        report.advance(1);
    }
    let pulse: &dyn Pulse = &NoPulse;
    old_stop_site(pulse).unwrap();
    old_report_site(pulse);
}

#[test]
fn steps_need_only_a_dyn_pulse_and_handle_success() {
    let pulse: &dyn Pulse = &NoPulse;
    let mut steps = Steps::new(
        pulse,
        &[
            PhaseSpec::new("decode", 2, Total::Exact(3)),
            PhaseSpec::new("encode", 1, Total::Exact(1)),
        ],
    )
    .unwrap();
    steps
        .run_stoppable(|stage| {
            stage.check()?;
            stage.advance(3);
            Ok::<(), StopReason>(())
        })
        .unwrap();
    steps
        .run(|stage| {
            stage.advance(1);
            Ok::<(), StopReason>(())
        })
        .unwrap();
    steps.finish().unwrap();
    let mut incomplete =
        Steps::new(pulse, &[PhaseSpec::new("pending", 1, Total::Unknown)]).unwrap();
    assert!(matches!(incomplete.run(|_| Ok::<(), ()>(())), Ok(())));
    assert!(matches!(
        incomplete.run(|_| Ok::<(), ()>(())),
        Err(RunError::Plan(PlanError::NoMoreChildren))
    ));
}

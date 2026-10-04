use how_far::{
    Complete, Execution, NoPulse, PhaseSpec, Phases, ResultExt, Stages, StopReason, Total,
};

#[test]
fn foreign_nonclone_error_is_borrowed_for_classification_and_returned_unchanged() {
    // Box<ForeignError> is representative of an external wrapper we cannot add
    // the desired TryFrom implementation to. Its allocation identity survives.
    struct ForeignError {
        reason: StopReason,
    }
    let original = Box::new(ForeignError {
        reason: StopReason::TimedOut,
    });
    let identity = (&*original) as *const _;
    let mut stages = Stages::new(&NoPulse, &[PhaseSpec::new("work", 1, Total::Unknown)]);
    let result = stages.run_classified::<(), Box<ForeignError>>(
        |e| e.reason == StopReason::TimedOut,
        |_| Err::<(), _>(original),
    );
    let error = stages
        .complete_classified(result, |e| e.reason == StopReason::TimedOut)
        .unwrap_err();
    assert_eq!((&*error) as *const _, identity);
}

#[test]
fn plain_stop_reason_works_and_independent_attempts_can_recover() {
    let mut phases = Phases::new(
        &NoPulse,
        Execution::Sequence,
        &[
            PhaseSpec::new("attempt", 1, Total::Unknown),
            PhaseSpec::new("fallback", 1, Total::Unknown),
        ],
    );
    let stopped = phases.run(0, |_| Err::<(), _>(StopReason::Cancelled));
    assert_eq!(stopped, Err(StopReason::Cancelled));
    let recovered = phases.run(1, |_| Ok::<_, StopReason>(42));
    // Recovery is the library's decision, even if it chooses to recover a stop.
    assert_eq!(recovered.finish_phase(phases), Ok(42));
}

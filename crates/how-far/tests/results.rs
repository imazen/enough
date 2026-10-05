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
fn complete_with_hands_over_an_early_question_mark() {
    use how_far::Outcome;
    let parts = [
        PhaseSpec::new("first", 1, Total::Unknown),
        PhaseSpec::new("second", 1, Total::Unknown),
    ];
    // An early error escapes the body but still reaches the owner, unchanged.
    let result = Stages::new(&NoPulse, &parts).complete_with(|stages| {
        stages.run(|_| Err::<(), _>(StopReason::TimedOut))?;
        stages.run(|_| Ok::<_, StopReason>(()))
    });
    assert_eq!(result, Err(StopReason::TimedOut));

    // The owner receives the body's own outcome.
    struct Recorder<'a>(&'a core::cell::Cell<Option<Outcome>>);
    impl Complete for Recorder<'_> {
        fn complete_as(self, outcome: Outcome) {
            self.0.set(Some(outcome));
        }
    }
    let seen = core::cell::Cell::new(None);
    let value = Recorder(&seen).complete_with(|_| Ok::<_, StopReason>(5));
    assert_eq!((value, seen.get()), (Ok(5), Some(Outcome::Succeeded)));
    let stopped = Recorder(&seen).complete_with(|_| Err::<(), _>(StopReason::Cancelled));
    assert_eq!(
        (stopped, seen.get()),
        (Err(StopReason::Cancelled), Some(Outcome::Cancelled))
    );
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

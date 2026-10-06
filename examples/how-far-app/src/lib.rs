//! An application adds tracking and diagnostics without changing either library.
use how_far::{Total, prelude::*};
use how_far_along::{Phase, PulseTree};
use how_far_really::{
    diagnostics::DiagnosticPulse,
    profile::{Profiler, StdClock, Trace},
};

pub fn observe<T, E>(
    stop: impl Stop + 'static,
    work: impl FnOnce(&dyn Pulse) -> Result<T, E>,
) -> (Result<T, E>, Trace)
where
    E: IsStop,
{
    let profiler = Profiler::new(StdClock::new(), 128);
    let pulse = DiagnosticPulse::new(
        PulseTree::new(Phase::new("operation", Total::Unknown), stop),
        &profiler,
    );
    let observer = pulse.observer();
    let result = work(&pulse).finish_phase(pulse);
    profiler.operation_returned();
    (
        result,
        profiler.snapshot().with_progress(observer.snapshot()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use almost_enough::Stopper;
    use how_far::{Child, Execution, Outcome, PhaseSpec, PlanError, StopReason, Unstoppable};
    use how_far_along::Status;
    use how_far_example_codec::{self as codec, Mode};
    use how_far_example_pipeline::{self as pipeline, Bug, Error};
    use how_far_really::diagnostics::{Kind, Options, Problem};

    fn kinds(trace: &Trace) -> Vec<Kind> {
        trace
            .diagnose(&Options::default())
            .iter()
            .map(|f| f.kind)
            .collect()
    }

    #[test]
    fn success_and_nested_recovery_keep_distinct_attempts() {
        for mode in [Mode::Fast, Mode::Fallback] {
            let (result, trace) =
                observe(Unstoppable, |p| pipeline::convert(&[1, 2], p, mode, false));
            assert_eq!(result, Ok(vec![254, 253]));
            let root = trace.progress.as_ref().unwrap();
            assert_eq!(root.status, Status::Finished(Outcome::Succeeded));
            assert_eq!(root.children[2].status, Status::Finished(Outcome::Skipped));
            assert!(root.children[2].completion_inferred);
            let attempts = &root.children[0].children;
            match mode {
                Mode::Fast => {
                    assert_eq!(attempts[0].status, Status::Finished(Outcome::Succeeded));
                    assert_eq!(attempts[1].status, Status::Finished(Outcome::Skipped));
                    assert!(!kinds(&trace).contains(&Kind::RecoveredFailure));
                }
                Mode::Fallback => {
                    assert_eq!(attempts[0].status, Status::Finished(Outcome::Failed));
                    assert_eq!(attempts[1].status, Status::Finished(Outcome::Succeeded));
                    assert!(kinds(&trace).contains(&Kind::RecoveredFailure));
                }
                _ => unreachable!(),
            }
            assert!(!kinds(&trace).contains(&Kind::AbandonedPhase));
            assert!(!kinds(&trace).contains(&Kind::CountMismatch));
        }
    }

    #[test]
    fn failures_and_cancellation_preserve_the_library_error_across_nesting() {
        for mode in [Mode::Fatal, Mode::FallbackFails] {
            let (result, trace) =
                observe(Unstoppable, |p| pipeline::convert(&[1, 2], p, mode, true));
            assert_eq!(
                result,
                Err(Error::Codec {
                    image: 0,
                    error: codec::Error::Corrupt { row: 0 }
                })
            );
            let root = trace.progress.unwrap();
            assert_eq!(root.status, Status::Finished(Outcome::Failed));
            assert_eq!(root.children[1].status, Status::Finished(Outcome::NotRun));
            assert_eq!(
                root.children[0].children[1].status,
                Status::Finished(match mode {
                    Mode::Fatal => Outcome::NotRun,
                    _ => Outcome::Failed,
                })
            );
        }
        let stop = Stopper::new();
        stop.cancel();
        let (result, trace) = observe(stop, |p| {
            pipeline::convert(&[1, 2], p, Mode::Fallback, true)
        });
        assert_eq!(
            result,
            Err(Error::Codec {
                image: 0,
                error: codec::Error::Stopped(StopReason::Cancelled)
            })
        );
        let root = trace.progress.unwrap();
        assert_eq!(root.status, Status::Finished(Outcome::Cancelled));
        assert_eq!(
            root.children[0].children[1].status,
            Status::Finished(Outcome::NotRun)
        );
    }

    #[test]
    fn omissions_change_observations_without_replacing_the_work_result() {
        for (bug, expected) in [
            (Bug::MissingHandoff, Kind::AbandonedPhase),
            (Bug::EarlyQuestionMark, Kind::AbandonedPhase),
            (Bug::MissingReports, Kind::CountMismatch),
            (Bug::DuplicateStage, Kind::ProtocolMisuse),
            (Bug::CountBeforePlan, Kind::ProtocolMisuse),
            (Bug::ReportToBranch, Kind::InvalidReport),
            (Bug::ForgottenRequiredStage, Kind::InferredCompletion),
        ] {
            let (result, trace) = observe(Unstoppable, |p| pipeline::buggy(p, bug));
            if matches!(bug, Bug::EarlyQuestionMark) {
                assert_eq!(result, Err(Error::Invalid("bad input")));
            } else {
                assert_eq!(result, Ok(()));
            }
            assert!(
                kinds(&trace).contains(&expected),
                "{bug:?}: {:?}",
                kinds(&trace)
            );
        }
    }

    #[test]
    fn swallowed_or_misclassified_cancellation_is_evidence_not_a_rewritten_result() {
        for bug in [Bug::SwallowedStop, Bug::MisclassifiedStop] {
            let stop = Stopper::new();
            stop.cancel();
            let (result, trace) = observe(stop, |p| pipeline::buggy(p, bug));
            assert_eq!(
                result,
                if matches!(bug, Bug::SwallowedStop) {
                    Ok(())
                } else {
                    Err(Error::Invalid("lost stop reason"))
                }
            );
            assert!(kinds(&trace).contains(&Kind::StopOutcomeMismatch));
        }
    }

    #[test]
    fn final_partial_batch_counts_even_when_the_library_forgets_to_check() {
        for finish in [false, true] {
            let stop = Stopper::new();
            let profiler = Profiler::new(StdClock::new(), 32);
            let pulse = DiagnosticPulse::new(
                PulseTree::new(Phase::new("batch", Total::Exact(2)), stop.clone()),
                &profiler,
            );
            let observer = pulse.observer();
            let result = pipeline::final_batch(
                &pulse,
                || {
                    stop.cancel();
                    profiler.cancellation_requested();
                },
                finish,
            )
            .finish_phase(pulse);
            profiler.operation_returned();
            let trace = profiler.snapshot().with_progress(observer.snapshot());
            assert_eq!(observer.snapshot().completed, 2);
            assert_eq!(
                result,
                if finish {
                    Err(Error::Stopped(StopReason::Cancelled))
                } else {
                    Ok(())
                }
            );
            assert_eq!(trace.observed_at.is_some(), finish);
            assert_eq!(
                kinds(&trace).contains(&Kind::UnobservedCancellation),
                !finish
            );
        }
    }

    #[test]
    fn late_shared_reports_are_diagnosed_without_changing_a_terminal_snapshot() {
        let profiler = Profiler::new(StdClock::new(), 1);
        let pulse = DiagnosticPulse::new(
            PulseTree::new(Phase::new("late", Total::Exact(1)), Unstoppable),
            &profiler,
        );
        let observer = pulse.observer();
        let shared = pulse.share().unwrap();
        let again = shared.share().unwrap();
        pulse.advance(1);
        pulse.complete(Ok::<_, StopReason>(())).unwrap();
        let before = observer.snapshot();
        shared.advance(2);
        again.advance(3);
        assert_eq!(observer.snapshot(), before);
        let trace = profiler.snapshot();
        assert_eq!(trace.incidents.len(), 1);
        assert_eq!(trace.dropped_incidents, 1);
        assert_eq!(trace.incidents[0].problem, Problem::ReportAfterCompletion);
        // Windows reports the path with backslashes.
        assert!(
            trace.incidents[0]
                .site
                .file
                .replace('\\', "/")
                .ends_with("how-far-app/src/lib.rs")
        );
    }

    #[test]
    fn caller_child_and_library_local_stop_compose_through_nested_libraries() {
        let local = Stopper::new();
        local.cancel();
        let (result, trace) = observe(Unstoppable, |root| {
            let mut stages = how_far::Stages::new(
                root,
                &[
                    PhaseSpec::new("encoder", 1, Total::Unknown),
                    PhaseSpec::new("save", 1, Total::Unknown),
                ],
            );
            let result = stages.run(|stage| {
                pipeline::convert_with_stop(&[1, 2], stage, &local, Mode::Fast, false)
            });
            result.finish_phase(stages)
        });
        assert_eq!(
            result,
            Err(Error::Codec {
                image: 0,
                error: codec::Error::Stopped(StopReason::Cancelled)
            })
        );
        let root = trace.progress.unwrap();
        assert_eq!(
            root.children[0].status,
            Status::Finished(Outcome::Cancelled)
        );
        assert_eq!(root.children[1].status, Status::Finished(Outcome::NotRun));
    }

    #[test]
    fn root_handoff_and_started_child_handoff_cannot_be_inferred_from_drop() {
        let profiler = Profiler::new(StdClock::new(), 32);
        let root = DiagnosticPulse::new(
            PulseTree::new(Phase::new("omitted", Total::Unknown), Unstoppable),
            &profiler,
        );
        let observer = root.observer();
        drop(root);
        let trace = profiler.snapshot().with_progress(observer.snapshot());
        assert!(kinds(&trace).contains(&Kind::AbandonedPhase));

        let (result, trace) = observe(Unstoppable, |p| {
            let child = p
                .split(
                    Execution::Sequence,
                    &[PhaseSpec::new("started", 1, Total::Unknown)],
                )
                .unwrap()
                .pop()
                .unwrap();
            child.start().unwrap();
            drop(child);
            Ok::<_, Error>(())
        });
        assert_eq!(result, Ok(()));
        let snapshot = trace.progress.as_ref().unwrap();
        assert_eq!(snapshot.status, Status::Finished(Outcome::Succeeded));
        assert_eq!(
            snapshot.children[0].status,
            Status::Finished(Outcome::Abandoned)
        );
        assert!(kinds(&trace).contains(&Kind::AbandonedPhase));
    }

    #[test]
    fn parent_handoff_freezes_inferred_states_without_rewriting_retained_children() {
        for result in [Ok(()), Err(StopReason::Cancelled)] {
            let mut root = Phase::new("parent", Total::Unknown);
            let observer = root.observer();
            let [untouched, started, mut recovered] = root
                .split(
                    Execution::ForkJoin,
                    [
                        PhaseSpec::new("untouched", 1, Total::Exact(1)),
                        PhaseSpec::new("started", 1, Total::Exact(2)),
                        PhaseSpec::new("recovered attempt", 1, Total::Unknown),
                    ],
                )
                .unwrap();
            started.reporter().advance(1);
            recovered.finish_with(Outcome::Failed).unwrap();
            assert_eq!(root.complete(result), result);
            let before = observer.snapshot();
            assert_eq!(
                before.children[0].status,
                Status::Finished(if result.is_ok() {
                    Outcome::Skipped
                } else {
                    Outcome::NotRun
                })
            );
            assert_eq!(
                before.children[1].status,
                Status::Finished(Outcome::Abandoned)
            );
            assert_eq!(before.children[2].status, Status::Finished(Outcome::Failed));
            assert!(before.children[0].completion_inferred);
            assert!(before.children[1].completion_inferred);
            assert!(!before.children[2].completion_inferred);
            // These are orphaned owners now: later reports/finishes cannot
            // retroactively change the enclosing operation's observation.
            started.reporter().advance(1);
            started.complete(Ok::<_, StopReason>(())).unwrap();
            untouched.complete(Ok::<_, StopReason>(())).unwrap();
            assert_eq!(observer.snapshot(), before);
        }
    }

    #[test]
    fn a_rejecting_sink_preserves_bare_stop_reason_and_never_panics_or_spins() {
        struct Rejecting;
        impl Stop for Rejecting {
            fn check(&self) -> Result<(), StopReason> {
                Err(StopReason::TimedOut)
            }
        }
        impl Report for Rejecting {
            fn advance(&self, _: u64) {}
        }
        impl Pulse for Rejecting {
            fn split(
                &self,
                _: Execution,
                _: &[PhaseSpec<'_>],
            ) -> Result<Vec<Child<'_>>, PlanError> {
                Err(PlanError::Unsupported)
            }
        }
        let mut phases = how_far::Phases::new(
            &Rejecting,
            Execution::Sequence,
            &[PhaseSpec::new("rejected", 1, Total::Unknown)],
        );
        let result: Result<(), StopReason> = phases.run(0, |p| p.check());
        assert_eq!(result.finish_phase(phases), Err(StopReason::TimedOut));
    }
}

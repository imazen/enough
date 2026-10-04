#![cfg(feature = "adapters")]
use almost_enough::Stopper;
use how_far::{
    Complete, Execution, PhaseSpec, PlanError, Pulse, Stop, StopReason, Total, Unstoppable,
    WithStop,
};
use how_far_along::{Outcome, Phase, PulseTree, Status};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct CountChecks(Arc<AtomicUsize>);
impl Stop for CountChecks {
    fn check(&self) -> Result<(), StopReason> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Err(StopReason::TimedOut)
    }
}

#[test]
fn borrowed_policies_apply_at_every_depth_and_keep_completion_ownership() {
    for replace in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let root = PulseTree::new(
            Phase::new("caller stage", Total::Unknown),
            CountChecks(calls.clone()),
        );
        let observer = root.observer();
        let local = Stopper::new();
        {
            let combined = if replace {
                WithStop::replacing_borrowed(&root, &local)
            } else {
                WithStop::borrowed(&root, &local)
            };
            assert!(matches!(combined.share(), Err(PlanError::NotShareable)));
            assert!(matches!(
                combined.try_handle(),
                Err(PlanError::NotShareable)
            ));
            let child = combined
                .split(
                    Execution::Sequence,
                    &[PhaseSpec::new("encoder", 1, Total::Unknown)],
                )
                .unwrap()
                .pop()
                .unwrap();
            let grandchild = child
                .split(
                    Execution::Sequence,
                    &[PhaseSpec::new("rows", 1, Total::Exact(1))],
                )
                .unwrap()
                .pop()
                .unwrap();
            assert_eq!(
                grandchild.check(),
                if replace {
                    Ok(())
                } else {
                    Err(StopReason::TimedOut)
                }
            );
            local.cancel();
            assert_eq!(
                grandchild.check(),
                Err(if replace {
                    StopReason::Cancelled
                } else {
                    StopReason::TimedOut
                })
            );
            // Counting is independent of the chosen checkpoint policy.
            how_far::Report::advance(&grandchild, 1);
            grandchild
                .complete(Err::<(), _>(StopReason::Cancelled))
                .unwrap_err();
            child
                .complete(Err::<(), _>(StopReason::Cancelled))
                .unwrap_err();
        }
        root.complete(Err::<(), _>(StopReason::Cancelled))
            .unwrap_err();
        let snapshot = observer.snapshot();
        assert_eq!(snapshot.children[0].children[0].completed, 1);
        assert_eq!(snapshot.status, Status::Finished(Outcome::Cancelled));
        assert_eq!(calls.load(Ordering::Relaxed), if replace { 0 } else { 2 });
    }
}

#[test]
fn combined_borrowed_stop_reaches_scoped_workers_and_original_policy_still_applies() {
    let original = Stopper::new();
    let local = Stopper::new();
    let root = PulseTree::new(Phase::new("work", Total::Unknown), original.clone());
    let combined = WithStop::borrowed(&root, &local);
    assert_eq!(combined.check(), Ok(()));
    std::thread::scope(|scope| {
        let borrowed = &combined;
        let local = &local;
        let worker = scope.spawn(move || {
            local.cancel();
            borrowed.check()
        });
        assert_eq!(worker.join().unwrap(), Err(StopReason::Cancelled));
    });
    let replaced = WithStop::replacing_borrowed(&root, &Unstoppable);
    original.cancel();
    assert_eq!(combined.check(), Err(StopReason::Cancelled));
    assert_eq!(replaced.check(), Ok(()));
    assert!(!replaced.may_stop());
}

#[test]
fn owned_replacement_and_combination_survive_spawn_and_new_children() {
    for replace in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let root = PulseTree::new(
            Phase::new("worker", Total::Unknown),
            CountChecks(calls.clone()),
        );
        let local = Stopper::new();
        let adapter = if replace {
            WithStop::replacing(&root, local.clone())
        } else {
            WithStop::new(&root, local.clone())
        };
        let shared = adapter.share().unwrap();
        let worker = std::thread::spawn(move || {
            let child = shared
                .split(
                    Execution::Sequence,
                    &[PhaseSpec::new("new child", 1, Total::Unknown)],
                )
                .unwrap()
                .pop()
                .unwrap();
            assert_eq!(
                child.check(),
                if replace {
                    Ok(())
                } else {
                    Err(StopReason::TimedOut)
                }
            );
            local.cancel();
            let result = child.check();
            assert_eq!(
                result,
                Err(if replace {
                    StopReason::Cancelled
                } else {
                    StopReason::TimedOut
                })
            );
            child.complete(result).unwrap_err();
        });
        worker.join().unwrap();
        drop(adapter);
        root.complete(Err::<(), _>(StopReason::Cancelled))
            .unwrap_err();
        assert_eq!(calls.load(Ordering::Relaxed), if replace { 0 } else { 2 });
    }
}

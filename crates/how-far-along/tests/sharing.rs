//! Shared views administer the phase they view without ever losing a plan,
//! a total revision, or a report to contention.

use how_far_along::{prelude::*, *};
use std::sync::{
    Arc, Barrier,
    atomic::{AtomicBool, Ordering},
};

fn tree(total: Total) -> PulseTree {
    PulseTree::new(Phase::new("job", total), Unstoppable)
}

#[test]
fn concurrent_total_revisions_from_shared_views_all_apply() {
    let tree = tree(Total::Estimated(1));
    std::thread::scope(|scope| {
        for worker in 0..4_u64 {
            let shared = tree.share().unwrap();
            scope.spawn(move || {
                for i in 0..2_000_u64 {
                    // Every revision applies; none is refused for contention.
                    shared
                        .set_total(Total::Estimated(worker * 1_000_000 + i + 2))
                        .unwrap();
                }
            });
        }
    });
    let snapshot = tree.observer().snapshot();
    assert_eq!(snapshot.total_revisions.len(), 16, "bounded history");
    assert_eq!(snapshot.total_revisions.last(), Some(&snapshot.total));
    tree.finish(Outcome::Succeeded).unwrap();
}

#[test]
fn a_plan_racing_shared_administration_is_always_tracked() {
    for _ in 0..300 {
        let tree = tree(Total::Unknown);
        let observer = tree.observer();
        let shared = tree.share().unwrap();
        let go = Arc::new(Barrier::new(2));
        let done = Arc::new(AtomicBool::new(false));
        let worker = {
            let (go, done) = (go.clone(), done.clone());
            std::thread::spawn(move || {
                go.wait();
                while !done.load(Ordering::Relaxed) {
                    shared.start().unwrap();
                }
            })
        };
        go.wait();
        let result = Stages::new(
            &tree,
            &[
                PhaseSpec::new("a", 1, Total::Exact(4)),
                PhaseSpec::new("b", 1, Total::Exact(4)),
            ],
        )
        .complete_with(|stages| {
            stages.run(|stage| stage.step(4))?;
            stages.run(|stage| stage.step(4))
        });
        done.store(true, Ordering::Relaxed);
        worker.join().unwrap();
        assert_eq!(result, Ok(()));
        let snapshot = observer.snapshot();
        assert_eq!(snapshot.children.len(), 2, "the plan was never rejected");
        assert!(
            snapshot
                .children
                .iter()
                .all(|stage| stage.status == Status::Finished(Outcome::Succeeded))
        );
        result.finish_phase(tree).unwrap();
    }
}

#[test]
fn a_report_racing_a_split_is_counted_or_rejected_never_lost() {
    for _ in 0..2_000 {
        let tree = tree(Total::Exact(1));
        let observer = tree.observer();
        let reporter = tree.share().unwrap();
        let go = Arc::new(Barrier::new(2));
        let worker = {
            let go = go.clone();
            std::thread::spawn(move || {
                go.wait();
                reporter.advance(1);
            })
        };
        go.wait();
        {
            let split = tree.split(
                Execution::Sequence,
                &[PhaseSpec::new("part", 1, Total::Exact(1))],
            );
            worker.join().unwrap();
            match split {
                // The split won: the late report went to a branch and is ignored.
                Ok(children) => {
                    for child in children {
                        child.finish(Outcome::Succeeded).unwrap();
                    }
                }
                // The report won and made this phase a counting leaf.
                Err(error) => {
                    assert_eq!(error, PlanError::AlreadyInUse);
                    assert_eq!(observer.snapshot().completed, 1, "report lost");
                }
            }
        }
        tree.finish(Outcome::Succeeded).unwrap();
    }
}

#[test]
fn splitting_discards_the_total_history_a_branch_no_longer_has() {
    let tree = tree(Total::Unknown);
    tree.set_total(Total::Exact(5)).unwrap();
    assert_eq!(
        tree.observer().snapshot().total_revisions,
        [Total::Exact(5)]
    );
    let children = tree
        .split(
            Execution::Sequence,
            &[PhaseSpec::new("a", 1, Total::Exact(1))],
        )
        .unwrap();
    let snapshot = tree.observer().snapshot();
    assert_eq!(snapshot.total, Total::Unknown);
    assert!(snapshot.total_revisions.is_empty());
    for child in children {
        child.finish(Outcome::Succeeded).unwrap();
    }
    tree.finish(Outcome::Succeeded).unwrap();
}

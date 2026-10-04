use how_far::{RunError, TryStages};
use how_far_along::{prelude::*, *};
#[cfg(feature = "callback")]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

fn tree(total: Total) -> PulseTree {
    PulseTree::new(Phase::new("job", total), Unstoppable)
}
fn leaf(p: &dyn Pulse, n: u64) -> Result<(), RunError<StopReason>> {
    let mut stages = TryStages::new(p, &[PhaseSpec::new("leaf", 1, Total::Exact(n))])?;
    stages.run_stoppable(|s| {
        s.check()?;
        let mut paced = s.paced(17);
        for _ in 0..n {
            paced.step(1)?;
        }
        paced.finish()
    })?;
    stages.finish()?;
    Ok(())
}
#[test]
fn owned_workers_can_call_nested_libraries_and_finish_only_their_children() {
    for threads in [1, 2, 4, 8] {
        let tree = tree(Total::Unknown);
        let observer = tree.observer();
        let parts = vec![PhaseSpec::new("worker", 1, Total::Unknown); threads];
        let children = tree.split(Execution::ForkJoin, &parts).unwrap();
        let workers: Vec<_> = children
            .iter()
            .map(|c| {
                let view = c.share().unwrap();
                std::thread::spawn(move || leaf(&view, 101).unwrap())
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        for child in children {
            child.finish(Outcome::Succeeded).unwrap();
        }
        tree.finish(Outcome::Succeeded).unwrap();
        let snapshot = observer.snapshot();
        assert_eq!(snapshot.fraction(), Some(1.0));
        for c in snapshot.children {
            assert_eq!(c.children[0].completed, 101);
        }
    }
}
#[test]
fn revisions_start_and_zero_counts_work_through_the_trait() {
    let tree = tree(Total::Unknown);
    let p: &dyn Pulse = &tree;
    p.advance(0);
    p.start().unwrap();
    let [child] = p
        .split_array(
            Execution::Sequence,
            [PhaseSpec::new("x", 1, Total::Estimated(10))],
        )
        .unwrap();
    child.advance(5);
    assert_eq!(tree.observer().summary().fraction, Some(0.5));
    child.set_total(Total::Exact(20)).unwrap();
    assert_eq!(tree.observer().summary().fraction, Some(0.25));
    child.finish(Outcome::Failed).unwrap();
    tree.finish(Outcome::Failed).unwrap();
}
#[test]
fn abandoned_and_finished_owners_freeze_even_with_shared_handles() {
    for outcome in [None, Some(Outcome::Succeeded), Some(Outcome::Failed)] {
        let tree = tree(Total::Exact(10));
        let view = tree.share().unwrap();
        let observer = tree.observer();
        tree.advance(2);
        if let Some(outcome) = outcome {
            tree.finish(outcome).unwrap();
        } else {
            drop(tree);
        }
        let before = observer.snapshot();
        view.advance(8);
        assert_eq!(observer.snapshot(), before);
        assert_eq!(view.set_total(Total::Exact(30)), Err(PlanError::Finished));
        assert_eq!(
            view.split(
                Execution::Sequence,
                &[PhaseSpec::new("late", 1, Total::Unknown)]
            )
            .err(),
            Some(PlanError::Finished)
        );
    }
}
#[test]
fn unrun_work_never_earns_progress() {
    let tree = tree(Total::Unknown);
    let mut stages = TryStages::new(
        &tree,
        &[
            PhaseSpec::new("validate", 1, Total::Exact(1)),
            PhaseSpec::new("encode", 99, Total::Unknown),
        ],
    )
    .unwrap();
    assert_eq!(
        stages.run(|_| Err::<(), _>("bad input")),
        Err(RunError::Work("bad input"))
    );
    let snap = tree.observer().snapshot();
    assert_eq!(snap.children[1].status, Status::Finished(Outcome::NotRun));
    assert_eq!(snap.fraction(), Some(0.0));
    assert_eq!(tree.observer().summary().fraction, Some(0.0));
}
#[test]
fn concurrent_reports_are_exact_after_join_and_overflow_saturates() {
    let tree = tree(Total::Exact(80_000));
    let observer = tree.observer();
    let barrier = Arc::new(Barrier::new(9));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let p = tree.share().unwrap();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..10_000 {
                    p.advance(1);
                }
            })
        })
        .collect();
    barrier.wait();
    for _ in 0..100 {
        let s = observer.try_summary();
        if let Some(s) = s {
            assert!(s.fraction.unwrap() <= 1.0);
        }
    }
    for w in workers {
        w.join().unwrap();
    }
    tree.finish(Outcome::Succeeded).unwrap();
    assert_eq!(observer.snapshot().completed, 80_000);
    let overflow = self::tree(Total::Exact(10));
    overflow.advance(u64::MAX);
    overflow.advance(1);
    overflow.advance(10);
    assert_eq!(overflow.observer().summary().fraction, None);
    assert_eq!(
        overflow.observer().snapshot().completed,
        Snapshot::counter_max()
    );
}
#[cfg(feature = "callback")]
#[test]
fn callbacks_only_run_at_checkpoints_and_final_paced_cancellation_propagates() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let p = FnPulse::with_phase(Phase::new("job", Total::Exact(1)), move |cx| {
        seen.fetch_add(1, Ordering::Relaxed);
        if cx.summary().fraction == Some(1.0) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    });
    p.check().unwrap();
    let view: &dyn Pulse = &p;
    let mut pace = view.paced(64);
    pace.step(1).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(pace.finish(), Err(StopReason::Cancelled));
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    p.check().unwrap_err();
    p.advance(0);
    p.finish(Outcome::Cancelled).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 2);
}
#[cfg(feature = "callback")]
#[test]
fn callback_reentry_for_observation_and_total_changes_holds_no_internal_lock() {
    let holder = Arc::new(std::sync::Mutex::new(None::<SharedPulse>));
    let shared = holder.clone();
    let p = FnPulse::new("job", move |cx| {
        assert!(cx.try_summary().is_some());
        shared
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .set_total(Total::Exact(10))
            .unwrap();
        Ok(())
    });
    *holder.lock().unwrap() = Some(p.share().unwrap());
    p.step(1).unwrap();
    assert_eq!(p.observer().summary().fraction, Some(0.1));
    holder.lock().unwrap().take();
    p.finish(Outcome::Succeeded).unwrap();
}
#[cfg(feature = "callback")]
#[test]
fn callback_drop_and_abandonment_never_call_user_code() {
    let p = FnPulse::new("job", |_| panic!("no checkpoint should run"));
    let view = p.share().unwrap();
    let observer = p.observer();
    {
        let mut paced = view.as_pulse().paced(10);
        paced.step(2).unwrap();
    }
    drop(p);
    let before = observer.snapshot();
    view.advance(100);
    assert_eq!(observer.snapshot(), before);
    view.check().unwrap();
}

#[test]
fn root_abandonment_racing_with_administration_freezes_after_the_administrator_exits() {
    for _ in 0..100 {
        let tree = tree(Total::Unknown);
        let observer = tree.observer();
        let view = tree.share().unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let other = barrier.clone();
        let worker = std::thread::spawn(move || {
            other.wait();
            if let Ok([child]) = view.split_array(
                Execution::Sequence,
                [PhaseSpec::new("late", 1, Total::Exact(10))],
            ) {
                child.advance(3);
                let _ = child.finish(Outcome::Succeeded);
            }
            view
        });
        barrier.wait();
        drop(tree);
        let view = worker.join().unwrap();
        let frozen = observer.snapshot();
        assert_eq!(frozen.status, Status::Finished(Outcome::Abandoned));
        view.advance(100);
        assert_eq!(observer.snapshot(), frozen);
        assert!(observer.try_summary().is_some());
    }
}
#[cfg(feature = "adapters")]
#[test]
fn added_stop_policies_survive_splitting_and_owned_nesting() {
    let stop = almost_enough::Stopper::new();
    let pulse = WithStop::stop_only(stop.clone());
    let [child] = pulse
        .split_array(
            Execution::Sequence,
            [PhaseSpec::new("x", 1, Total::Unknown)],
        )
        .unwrap();
    let shared = child.share().unwrap();
    leaf(&shared, 20).unwrap();
    stop.cancel();
    assert_eq!(child.check(), Err(StopReason::Cancelled));
    assert_eq!(shared.check(), Err(StopReason::Cancelled));
    child.finish(Outcome::Cancelled).unwrap();
}
#[cfg(feature = "callback")]
#[test]
fn concurrent_callbacks_latch_one_reason_and_return_it_to_every_worker() {
    let barrier = Arc::new(Barrier::new(4));
    let rendezvous = barrier.clone();
    let entered = Arc::new(AtomicUsize::new(0));
    let calls = entered.clone();
    let pulse = FnPulse::new("job", move |_| {
        let n = calls.fetch_add(1, Ordering::Relaxed);
        rendezvous.wait();
        Err(if n & 1 == 0 {
            StopReason::Cancelled
        } else {
            StopReason::TimedOut
        })
    });
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let p = pulse.share().unwrap();
            std::thread::spawn(move || p.check())
        })
        .collect();
    let reasons: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert!(reasons.iter().all(|r| *r == reasons[0]));
    assert_eq!(pulse.check(), reasons[0]);
    assert_eq!(entered.load(Ordering::Relaxed), 4);
}
#[test]
fn intentionally_skipped_work_is_distinct_from_work_prevented_by_failure() {
    let tree = tree(Total::Unknown);
    let mut stages = TryStages::new(
        &tree,
        &[
            PhaseSpec::new("optional", 1, Total::Unknown),
            PhaseSpec::new("work", 1, Total::Exact(1)),
        ],
    )
    .unwrap();
    stages.skip().unwrap();
    assert_eq!(tree.observer().summary().fraction, Some(0.5));
    stages.run_stoppable(|s| s.step(1)).unwrap();
    stages.finish().unwrap();
    tree.finish(Outcome::Succeeded).unwrap();
}

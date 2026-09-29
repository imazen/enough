use howfar_along::ext::ReportExt;
use howfar_along::{Execution, Outcome, Part, Phase, PlanError, Report, Snapshot, Status, Total};
use rayon::prelude::*;
use std::{
    num::NonZeroU64,
    sync::{Arc, Barrier, mpsc},
};

fn near(a: Option<f64>, b: f64) {
    assert!((a.unwrap() - b).abs() < 1e-12, "{a:?} != {b}");
}

#[test]
fn serial_parallel_join_serial_keeps_middle_budget_at_thirty_percent() {
    let mut job = Phase::new("job", Total::Unknown);
    let observer = job.observer();
    let [mut before, mut middle, mut after] = job
        .split(
            Execution::Sequence,
            [
                Part::new("before", 35, Total::Exact(1)),
                Part::new("middle", 30, Total::Unknown),
                Part::new("after", 35, Total::Exact(1)),
            ],
        )
        .unwrap();
    let [mut small, mut slow] = middle
        .split(
            Execution::ForkJoin,
            [
                Part::new("small", 1, Total::Exact(1)),
                Part::new("slow", 1, Total::Exact(100)),
            ],
        )
        .unwrap();
    before.finish().unwrap();
    near(observer.snapshot().fraction(), 0.35);
    assert!(!middle.progress().may_report());
    middle.progress().advance(999); // Branch work never double counts.
    let (done_tx, done_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            small.progress().advance(1);
            small.finish().unwrap();
            done_tx.send(()).unwrap();
        });
        scope.spawn(move || {
            release_rx.recv().unwrap();
            slow.progress().advance(100);
            slow.finish().unwrap();
        });
        done_rx.recv().unwrap();
        near(observer.snapshot().fraction(), 0.50);
        assert_eq!(middle.finish(), Err(PlanError::UnfinishedChildren));
        assert_eq!(job.finish(), Err(PlanError::UnfinishedChildren));
        release_tx.send(()).unwrap();
    });
    middle.finish().unwrap();
    near(observer.snapshot().fraction(), 0.65);
    after.finish().unwrap();
    assert_eq!(observer.snapshot().status, Status::Running); // Counted 100% is not an outcome.
    job.finish().unwrap();
    assert_eq!(
        observer.snapshot().status,
        Status::Finished(Outcome::Succeeded)
    );
}

#[test]
fn repeated_nested_joins_and_skipped_reduction() {
    let mut job = Phase::new("job", Total::Unknown);
    let mut groups = job
        .split(
            Execution::Sequence,
            [
                Part::new("first", 1, Total::Unknown),
                Part::new("second", 1, Total::Unknown),
            ],
        )
        .unwrap();
    for group in &mut groups {
        let [mut fork, mut reduce] = group
            .split(
                Execution::Sequence,
                [
                    Part::new("fork", 9, Total::Unknown),
                    Part::new("reduce", 1, Total::Unknown),
                ],
            )
            .unwrap();
        let children = fork
            .split_vec(
                Execution::ForkJoin,
                (0..4)
                    .map(|i| Part::new(format!("part{i}"), i + 1, Total::Exact(i)))
                    .collect(),
            )
            .unwrap();
        std::thread::scope(|scope| {
            for mut child in children {
                scope.spawn(move || child.finish().unwrap());
            }
        });
        fork.finish().unwrap();
        reduce.finish_with(Outcome::Skipped).unwrap();
        group.finish().unwrap();
    }
    job.finish().unwrap();
    near(job.observer().snapshot().fraction(), 1.0);
}

#[test]
fn rayon_dynamic_asymmetric_tasks_share_one_count_and_flush_batches() {
    let mut job = Phase::new("pool", Total::Exact(1001));
    job.set_execution(Execution::WorkPool { max_parallelism: 4 })
        .unwrap();
    let progress = job.progress();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap();
    pool.install(|| {
        (0..1001).into_par_iter().for_each_init(
            || progress.clone().batched(NonZeroU64::new(16).unwrap()),
            |batch, item| {
                for i in 0..item % 19 {
                    std::hint::black_box(i * item);
                }
                batch.advance(1);
            },
        );
    });
    assert_eq!(job.observer().snapshot().completed, 1001);
    job.finish().unwrap();
}

#[test]
fn manual_workers_sum_exactly_and_terminal_snapshot_ignores_stale_reporters() {
    let mut job = Phase::new("manual", Total::Exact(8000));
    let progress = Arc::new(job.progress());
    let barrier = Arc::new(Barrier::new(8));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let progress = &progress;
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                for _ in 0..1000 {
                    progress.advance(1);
                }
            });
        }
    });
    job.finish().unwrap();
    let final_snapshot = job.observer().snapshot();
    assert_eq!(final_snapshot.completed, 8000);
    progress.advance(u64::MAX);
    assert_eq!(job.observer().snapshot(), final_snapshot);
    assert_eq!(job.set_total(Total::Exact(10)), Err(PlanError::Finished));
    let fresh_attempt = Phase::new("manual retry", Total::Exact(8000));
    assert_eq!(fresh_attempt.observer().snapshot().completed, 0);
}

#[test]
fn exact_estimated_unknown_zero_overrun_and_overflow_are_distinct() {
    let mut job = Phase::new("unknown", Total::Unknown);
    let progress = job.progress();
    progress.advance(4);
    assert_eq!(job.observer().snapshot().fraction(), None);
    job.set_total(Total::Estimated(8)).unwrap();
    near(job.observer().snapshot().fraction(), 0.5);
    job.set_total(Total::Estimated(16)).unwrap();
    near(job.observer().snapshot().fraction(), 0.25);
    job.set_total(Total::Exact(2)).unwrap();
    let snapshot = job.observer().snapshot();
    assert_eq!(snapshot.initial_total, Total::Unknown);
    assert_eq!(snapshot.total_revisions.len(), 3);
    assert!(snapshot.overrun());
    assert_eq!(snapshot.status, Status::Running);
    let mut zero = Phase::new("empty", Total::Exact(0));
    near(zero.observer().snapshot().fraction(), 0.0);
    zero.finish().unwrap();
    near(zero.observer().snapshot().fraction(), 1.0);
    let huge = Phase::new("overflow", Total::Unknown);
    huge.progress().advance(Snapshot::counter_max());
    assert!(!huge.observer().snapshot().overflowed);
    huge.progress().advance(1);
    assert!(huge.observer().snapshot().overflowed);
    assert_eq!(
        huge.observer().snapshot().completed,
        Snapshot::counter_max()
    );
    assert_eq!(huge.observer().snapshot().fraction(), None);
}

#[test]
fn unknown_subtree_keeps_reserved_budget_and_failures_do_not_discharge_it() {
    let mut job = Phase::new("job", Total::Unknown);
    let [mut known, mut unknown] = job
        .split(
            Execution::Sequence,
            [
                Part::new("known", 7, Total::Exact(10)),
                Part::new("discovery", 3, Total::Unknown),
            ],
        )
        .unwrap();
    known.finish().unwrap();
    assert_eq!(job.observer().snapshot().fraction(), None);
    assert!((job.observer().snapshot().unresolved_fraction() - 0.3).abs() < 1e-12);
    unknown.progress().advance(2);
    unknown.finish_with(Outcome::Cancelled).unwrap();
    assert_eq!(job.finish(), Err(PlanError::UnsuccessfulChildren));
    job.finish_with(Outcome::Cancelled).unwrap();
    assert_eq!(job.observer().snapshot().fraction(), None);
}

#[test]
fn invalid_plan_is_transactional_and_units_cannot_change_after_use() {
    let mut job = Phase::new("plan", Total::Unknown);
    assert!(matches!(
        job.split(Execution::Sequence, []),
        Err(PlanError::EmptyOrZeroWeight)
    ));
    assert!(matches!(
        job.split(Execution::Sequence, [Part::new("zero", 0, Total::Unknown)]),
        Err(PlanError::EmptyOrZeroWeight)
    ));
    assert!(matches!(
        job.split(
            Execution::Sequence,
            [
                Part::new("a", u64::MAX, Total::Unknown),
                Part::new("b", 1, Total::Unknown)
            ]
        ),
        Err(PlanError::Overflow)
    ));
    assert_eq!(
        job.set_execution(Execution::WorkPool { max_parallelism: 0 }),
        Err(PlanError::ZeroParallelism)
    );
    job.set_units("bytes").unwrap();
    let _progress = job.progress();
    assert_eq!(job.set_units("rows"), Err(PlanError::AlreadyInUse));
    assert!(matches!(
        job.split(Execution::Sequence, [Part::new("a", 1, Total::Unknown)]),
        Err(PlanError::AlreadyInUse)
    ));
}

#[test]
fn abandoning_owner_freezes_even_if_child_owner_lives_on() {
    let mut job = Phase::new("parent", Total::Unknown);
    let observer = job.observer();
    let [mut child] = job
        .split(
            Execution::Sequence,
            [Part::new("child", 1, Total::Exact(3))],
        )
        .unwrap();
    let progress = child.progress();
    progress.advance(1);
    drop(job);
    let frozen = observer.snapshot();
    assert_eq!(frozen.status, Status::Finished(Outcome::Abandoned));
    progress.advance(2);
    child.finish().unwrap();
    assert_eq!(observer.snapshot(), frozen);
}

#[test]
fn snapshots_during_metadata_publication_keep_each_revision_coherent() {
    let mut job = Phase::new("updates", Total::Estimated(1));
    let observer = job.observer();
    let progress = job.progress();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..500 {
                let snapshot = observer.snapshot();
                assert_eq!(
                    snapshot.total,
                    snapshot
                        .total_revisions
                        .last()
                        .copied()
                        .unwrap_or(snapshot.initial_total)
                );
            }
        });
        scope.spawn(move || {
            for _ in 0..500 {
                progress.advance(1);
            }
        });
        for n in 2..100 {
            job.set_total(Total::Estimated(n)).unwrap();
        }
    });
    job.finish().unwrap();
    assert_eq!(observer.snapshot().completed, 500);
}

#[test]
fn sharing_contract_and_owned_trait_objects() {
    fn shared<T: Send + Sync>() {}
    shared::<Phase>();
    shared::<howfar_along::Progress>();
    shared::<howfar_along::Observer>();
    let phase = Phase::new("erased", Total::Exact(5));
    let boxed: Box<dyn Report> = Box::new(phase.progress());
    let arc: Arc<dyn Report> = Arc::new(phase.progress());
    boxed.advance(2);
    arc.advance(3);
    assert_eq!(phase.observer().snapshot().completed, 5);
}

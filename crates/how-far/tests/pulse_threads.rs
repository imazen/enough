//! A hand-written `Pulse` shared by OS threads, with no tracker involved.

use how_far::{
    Child, Execution, PhaseSpec, PlanError, ProgressExt, Pulse, Report, SharedPulse, Stop,
    StopReason,
};
use std::sync::{
    Arc, Barrier,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

/// A leaf whose state is shared, so it can also hand out `'static` views.
#[derive(Clone, Default)]
struct SharedLeaf {
    completed: Arc<AtomicU64>,
    cancelled: Arc<AtomicBool>,
}

struct Flag(Arc<AtomicBool>);
impl Stop for Flag {
    fn check(&self) -> Result<(), StopReason> {
        if self.0.load(Ordering::Acquire) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }
}

impl Stop for SharedLeaf {
    fn check(&self) -> Result<(), StopReason> {
        Flag(self.cancelled.clone()).check()
    }
}
impl Report for SharedLeaf {
    fn advance(&self, units: u64) {
        self.completed.fetch_add(units, Ordering::Relaxed);
    }
}
impl Pulse for SharedLeaf {
    fn split(&self, _: Execution, _: &[PhaseSpec<'_>]) -> Result<Vec<Child<'_>>, PlanError> {
        Err(PlanError::Unsupported)
    }
    fn share(&self) -> Result<SharedPulse, PlanError> {
        Ok(SharedPulse::new(self.clone()))
    }
}

#[test]
fn four_scoped_threads_share_one_dyn_pulse_and_all_see_cancellation() {
    let leaf = SharedLeaf::default();
    let pulse: &dyn Pulse = &leaf;
    let start = Barrier::new(5);
    let reported = Barrier::new(5);
    let release = Barrier::new(5);

    std::thread::scope(|scope| {
        let workers: Vec<_> = [1_u64, 2, 3, 5]
            .into_iter()
            .map(|units| {
                let (start, reported, release) = (&start, &reported, &release);
                scope.spawn(move || {
                    start.wait();
                    pulse.check().unwrap();
                    pulse.step(units).unwrap();
                    reported.wait();
                    release.wait();
                    pulse.check()
                })
            })
            .collect();

        start.wait();
        reported.wait();
        let completed_before_cancel = leaf.completed.load(Ordering::Relaxed);
        leaf.cancelled.store(true, Ordering::Release);
        release.wait();

        assert_eq!(completed_before_cancel, 11);
        for worker in workers {
            assert_eq!(worker.join().unwrap(), Err(StopReason::Cancelled));
        }
    });
    assert_eq!(leaf.completed.load(Ordering::Relaxed), 11);
}

#[test]
fn spawned_static_threads_work_through_shared_views() {
    let leaf = SharedLeaf::default();
    let pulse: &dyn Pulse = &leaf;
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let shared = pulse.share().unwrap();
            // `thread::spawn` needs `'static`; a borrowed pulse cannot move in.
            std::thread::spawn(move || {
                for _ in 0..25 {
                    shared.step(1)?;
                }
                Ok::<(), StopReason>(())
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap().unwrap();
    }
    assert_eq!(leaf.completed.load(Ordering::Relaxed), 100);

    leaf.cancelled.store(true, Ordering::Release);
    let shared = pulse.share().unwrap();
    let stopped = std::thread::spawn(move || shared.check());
    assert_eq!(stopped.join().unwrap(), Err(StopReason::Cancelled));
}

#[test]
fn a_leaf_that_cannot_plan_says_so() {
    let leaf = SharedLeaf::default();
    assert_eq!(
        leaf.split(Execution::Sequence, &[]).err(),
        Some(PlanError::Unsupported)
    );
}

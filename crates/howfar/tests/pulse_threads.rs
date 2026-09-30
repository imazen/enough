use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use enough::{Stop, StopReason};
use howfar::{Execution, Outcome, PhaseSpec, PlanError, Pulse, Report};
use std::sync::Barrier;

/// A test-only leaf: this exercises the public trait without depending on the tracker.
#[derive(Default)]
struct SharedLeaf {
    completed: AtomicU64,
    cancelled: AtomicBool,
}
impl Stop for SharedLeaf {
    fn check(&self) -> Result<(), StopReason> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }
}
impl Report for SharedLeaf {
    fn advance(&self, units: u64) {
        self.completed.fetch_add(units, Ordering::Relaxed);
    }
}
impl Pulse for SharedLeaf {
    fn split(
        &self,
        _: Execution,
        _: &[PhaseSpec<'_>],
    ) -> Result<Vec<Box<dyn Pulse + '_>>, PlanError> {
        Err(PlanError::AlreadyInUse)
    }

    fn finish(&self, _: Outcome) -> Result<(), PlanError> {
        Ok(())
    }
}

#[test]
fn four_os_threads_share_one_dyn_pulse_and_see_cancellation() {
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
                    pulse.advance(units);
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

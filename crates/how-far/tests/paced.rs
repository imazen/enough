//! `Paced`: local counting, reaching the pulse only every so many units.

use how_far::{Child, Execution, Paced, PhaseSpec, PlanError, Pulse, Report, Stop, StopReason};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Records what reaches it: checks, reports, and the last call site.
#[derive(Default)]
struct Probe {
    silent: bool,
    stopped: AtomicBool,
    checks: AtomicU64,
    reports: AtomicU64,
    units: AtomicU64,
    line: Mutex<u32>,
}
impl Stop for Probe {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.checks.fetch_add(1, Ordering::Relaxed);
        *self.line.lock().unwrap() = std::panic::Location::caller().line();
        if self.stopped.load(Ordering::Relaxed) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }
    fn may_stop(&self) -> bool {
        !self.silent
    }
}
impl Report for Probe {
    #[track_caller]
    fn advance(&self, completed: u64) {
        self.reports.fetch_add(1, Ordering::Relaxed);
        self.units.fetch_add(completed, Ordering::Relaxed);
        *self.line.lock().unwrap() = std::panic::Location::caller().line();
    }
    fn may_report(&self) -> bool {
        !self.silent
    }
}
impl Pulse for Probe {
    fn split(&self, _: Execution, _: &[PhaseSpec<'_>]) -> Result<Vec<Child<'_>>, PlanError> {
        Err(PlanError::Unsupported)
    }
}
impl Probe {
    fn counts(&self) -> (u64, u64, u64) {
        (
            self.checks.load(Ordering::Relaxed),
            self.reports.load(Ordering::Relaxed),
            self.units.load(Ordering::Relaxed),
        )
    }
}

#[test]
fn a_pulse_that_cannot_stop_or_report_is_never_reached() {
    let probe = Probe {
        silent: true,
        ..Probe::default()
    };
    {
        let pulse: &dyn Pulse = &probe;
        let mut pace = pulse.paced(1);
        for _ in 0..1000 {
            pace.step(u64::MAX).unwrap();
        }
        pace.check().unwrap();
        pace.flush();
    }
    assert_eq!(probe.counts(), (0, 0, 0));
}

#[test]
fn a_live_pulse_is_reached_once_every_n_units_with_exact_counts() {
    let probe = Probe::default();
    {
        let mut pace = Paced::new(&probe, 10);
        for _ in 0..7 {
            pace.step(3).unwrap(); // Reached once, at 12 units.
        }
        assert_eq!(probe.counts(), (1, 1, 12));
    } // Dropping reports the 9 still pending, without checking.
    assert_eq!(probe.counts(), (1, 2, 21));
}

#[test]
fn a_stop_is_seen_at_the_next_reach_and_the_finished_work_is_counted() {
    let probe = Probe::default();
    let mut pace = Paced::new(&probe, 4);
    pace.step(3).unwrap();
    probe.stopped.store(true, Ordering::Relaxed);
    pace.step(2).unwrap_err();
    assert_eq!(probe.counts(), (1, 1, 5));
    // `check` does not wait for the pace.
    assert_eq!(pace.check(), Err(StopReason::Cancelled));
    assert_eq!(probe.counts(), (2, 1, 5));
}

#[test]
fn units_pending_at_an_early_return_are_still_counted() {
    fn work(pulse: &dyn Pulse, fail_at: u64) -> Result<(), &'static str> {
        let mut pace = pulse.paced(100);
        for row in 0..10 {
            if row == fail_at {
                return Err("corrupt row");
            }
            pace.step(1).map_err(|_| "stopped")?;
        }
        Ok(())
    }
    let probe = Probe::default();
    assert_eq!(work(&probe, 7), Err("corrupt row"));
    assert_eq!(probe.counts(), (0, 1, 7));
}

#[test]
fn flush_reports_without_checking_and_zero_means_every_unit() {
    let probe = Probe::default();
    let mut pace = Paced::new(&probe, 0);
    pace.step(1).unwrap();
    pace.step(1).unwrap();
    assert_eq!(probe.counts(), (2, 2, 2));
    let mut pace = Paced::new(&probe, 5);
    pace.step(2).unwrap();
    pace.flush();
    pace.flush(); // Nothing pending: nothing reported.
    assert_eq!(probe.counts(), (2, 3, 4));
}

#[test]
fn pending_units_saturate_instead_of_wrapping() {
    let probe = Probe::default();
    let mut pace = Paced::new(&probe, u64::MAX);
    pace.step(u64::MAX - 1).unwrap();
    pace.step(5).unwrap(); // Saturates at the threshold and reaches.
    assert_eq!(probe.counts(), (1, 1, u64::MAX));
}

#[test]
fn the_pulse_sees_the_step_call_site() {
    let probe = Probe::default();
    let mut pace = Paced::new(&probe, 1);
    let line = line!() + 1;
    pace.step(1).unwrap();
    assert_eq!(*probe.line.lock().unwrap(), line);
    let line = line!() + 1;
    pace.check().unwrap();
    assert_eq!(*probe.line.lock().unwrap(), line);
}

#[test]
fn an_owned_worker_paces_through_its_shared_view() {
    let probe = std::sync::Arc::new(Probe::default());
    let shared = how_far::SharedPulse::new(std::sync::Arc::clone(&probe));
    let worker = std::thread::spawn(move || {
        let mut pace = shared.paced(10);
        for _ in 0..25 {
            pace.step(1)?;
        }
        pace.finish()
    });
    assert_eq!(worker.join().unwrap(), Ok(()));
    // Two full batches, then the final partial batch flushed by finish.
    assert_eq!(probe.counts(), (3, 3, 25));
}

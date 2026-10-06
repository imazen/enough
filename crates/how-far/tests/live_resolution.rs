//! `live()` means two things: `ProgressExt::live` on anything that stops and
//! reports, and `enough`'s inherent `live` on `dyn Stop`. With the prelude in
//! scope, each receiver must reach its own, never fail to compile as
//! ambiguous, and never silently take the other's.

// `&Box<dyn _>` receivers are cases under test.
#![allow(clippy::borrowed_box)]

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use how_far::prelude::*;
use how_far::{Child, Execution, NoPulse, PhaseSpec, PlanError, StopReason};

/// Reports, and can never stop.
struct Counting(AtomicU64);

impl Stop for Counting {
    fn check(&self) -> Result<(), StopReason> {
        Ok(())
    }
    fn may_stop(&self) -> bool {
        false
    }
}

impl Report for Counting {
    fn advance(&self, completed: u64) {
        self.0.fetch_add(completed, Relaxed);
    }
}

impl Pulse for Counting {
    fn split(&self, _: Execution, _: &[PhaseSpec<'_>]) -> Result<Vec<Child<'_>>, PlanError> {
        Err(PlanError::Unsupported)
    }
}

// The return types pin which `live` each receiver resolves to: the inherent
// one returns `Option<&dyn Stop>`, `ProgressExt`'s `Option<&Self>`.
fn pulse_live(p: &dyn Pulse) -> Option<&dyn Pulse> {
    p.live()
}
fn send_pulse_live(p: &(dyn Pulse + Send)) -> Option<&(dyn Pulse + Send)> {
    p.live()
}
fn boxed_pulse_live(p: &Box<dyn Pulse>) -> Option<&Box<dyn Pulse>> {
    p.live()
}
fn generic_live<P: Pulse + ?Sized>(p: &P) -> Option<&P> {
    p.live()
}
fn stop_live(s: &dyn Stop) -> Option<&dyn Stop> {
    s.live()
}
fn send_sync_stop_live(s: &(dyn Stop + Send + Sync)) -> Option<&(dyn Stop + Send + Sync)> {
    s.live()
}
fn boxed_stop_live(s: &Box<dyn Stop>) -> Option<&dyn Stop> {
    s.live()
}

#[test]
fn a_pulse_that_only_reports_is_live_as_a_pulse_and_not_as_a_stop() {
    let counting = Counting(AtomicU64::new(0));
    let pulse: &dyn Pulse = &counting;

    // `ProgressExt::live`: live, because it reports.
    assert!(pulse_live(pulse).is_some());
    assert!(send_pulse_live(&counting).is_some());
    let via_ref: Option<&&dyn Pulse> = (&pulse).live();
    assert!(via_ref.is_some());
    assert!(generic_live(&counting).is_some());
    assert!(boxed_pulse_live(&(Box::new(Counting(AtomicU64::new(0))) as Box<dyn Pulse>)).is_some());

    // Inherent `dyn Stop::live`: not live, because it can never stop.
    let stop: &dyn Stop = pulse;
    assert!(stop_live(stop).is_none());
    assert!(send_sync_stop_live(&counting).is_none());
    assert!(boxed_stop_live(&(Box::new(Counting(AtomicU64::new(0))) as Box<dyn Stop>)).is_none());

    // The live pulse still counts.
    pulse.live().unwrap().step(3).unwrap();
    assert_eq!(counting.0.load(Relaxed), 3);
}

#[test]
fn no_pulse_is_dead_both_ways() {
    let pulse: &dyn Pulse = &NoPulse;
    assert!(pulse_live(pulse).is_none());
    assert!(stop_live(pulse).is_none());
}

#[test]
fn a_stoppable_stop_is_live_through_the_inherent_method() {
    struct Stoppable;
    impl Stop for Stoppable {
        fn check(&self) -> Result<(), StopReason> {
            Ok(())
        }
    }
    assert!(stop_live(&Stoppable).is_some());
}

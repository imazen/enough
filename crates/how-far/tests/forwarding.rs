//! A reference, box or `Arc` of a pulse is a pulse, so code that takes a pulse
//! by value or owns one can hold any pulse, `NoPulse` included.

use how_far::{
    Child, Execution, NoPulse, PhaseSpec, PlanError, ProgressExt, ProgressWithStop, Pulse,
    PulseHandle, Report, Stop, StopReason, Total,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Stops on request, counts, plans inert children, and hands out a handle.
#[derive(Default)]
struct Flagged {
    stopped: Arc<AtomicBool>,
    units: Arc<AtomicU64>,
}
struct Flag(Arc<AtomicBool>);
impl Stop for Flag {
    fn check(&self) -> Result<(), StopReason> {
        if self.0.load(Ordering::Relaxed) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }
}
struct Units(Arc<AtomicU64>);
impl Report for Units {
    fn advance(&self, completed: u64) {
        self.0.fetch_add(completed, Ordering::Relaxed);
    }
}
impl Stop for Flagged {
    fn check(&self) -> Result<(), StopReason> {
        Flag(self.stopped.clone()).check()
    }
}
impl Report for Flagged {
    fn advance(&self, completed: u64) {
        self.units.fetch_add(completed, Ordering::Relaxed);
    }
}
impl Pulse for Flagged {
    fn split(
        &self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<Vec<Child<'_>>, PlanError> {
        NoPulse.split(execution, parts)
    }
    fn handle(&self) -> PulseHandle {
        ProgressWithStop::new(
            Some(Arc::new(Flag(self.stopped.clone()))),
            Some(Arc::new(Units(self.units.clone()))),
        )
    }
}

/// Generic code that takes its pulse by value.
fn two_stages<P: Pulse>(pulse: P) -> Result<usize, PlanError> {
    let parts = [
        PhaseSpec::new("a", 1, Total::Exact(1)),
        PhaseSpec::new("b", 1, Total::Exact(1)),
    ];
    Ok(pulse.split(Execution::Sequence, &parts)?.len())
}

#[test]
fn generic_code_takes_any_pulse_by_value() {
    assert_eq!(two_stages(&NoPulse), Ok(2));
    assert_eq!(two_stages(Box::new(&NoPulse)), Ok(2));
    assert_eq!(two_stages(Arc::new(&NoPulse)), Ok(2));
    let mut flagged = Flagged::default();
    assert_eq!(two_stages(&flagged), Ok(2));
    assert_eq!(two_stages(&mut flagged), Ok(2));
    assert_eq!(two_stages(Box::new(flagged)), Ok(2));
}

#[test]
fn owned_pulses_can_default_to_no_pulse() {
    struct Options {
        pulse: Box<dyn Pulse>,
    }
    impl Default for Options {
        fn default() -> Self {
            Self {
                pulse: Box::new(&NoPulse),
            }
        }
    }
    let options = Options::default();
    options.pulse.step(3).unwrap();
    assert!(options.pulse.handle().stop.is_none());
    assert_eq!(
        options.pulse.split(Execution::Sequence, &[]).err(),
        Some(PlanError::EmptyOrZeroWeight)
    );
    let shared: Arc<dyn Pulse> = Arc::new(&NoPulse);
    std::thread::scope(|scope| {
        scope.spawn(|| shared.step(1).unwrap());
    });
}

#[test]
fn wrappers_forward_stops_reports_splits_and_handles() {
    let flagged = Arc::new(Flagged::default());
    let wrapped: [Box<dyn Pulse>; 3] = [
        Box::new(Arc::clone(&flagged)),
        Box::new(Box::new(Arc::clone(&flagged))),
        Box::new(Arc::new(Arc::clone(&flagged))),
    ];
    for pulse in &wrapped {
        pulse.step(2).unwrap();
        assert_eq!(two_stages(&**pulse), Ok(2));
        let handle = pulse.handle();
        handle.advance(1);
        assert!(handle.stop.is_some());
    }
    assert_eq!(flagged.units.load(Ordering::Relaxed), 9);
    flagged.stopped.store(true, Ordering::Relaxed);
    for pulse in &wrapped {
        assert_eq!(pulse.check(), Err(StopReason::Cancelled));
        assert_eq!(pulse.handle().check(), Err(StopReason::Cancelled));
    }
}

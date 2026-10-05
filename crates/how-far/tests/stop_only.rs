//! `StopOnly`: an application's cancellation token as a `&dyn Pulse`.

use how_far::{
    Execution, NoPulse, Outcome, PhaseSpec, PlanError, Stages, StopOnly, StopReason, Total,
    prelude::*,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

/// A token the application cancels; it counts the checks that reach it.
#[derive(Clone, Default)]
struct Token {
    cancelled: Arc<AtomicBool>,
    checks: Arc<AtomicUsize>,
}
impl Stop for Token {
    fn check(&self) -> Result<(), StopReason> {
        self.checks.fetch_add(1, Ordering::Relaxed);
        if self.cancelled.load(Ordering::Relaxed) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// A library with a nested plan that knows nothing about the token.
fn library(rows: u64, pulse: &dyn Pulse) -> Result<u64, StopReason> {
    Stages::new(
        pulse,
        &[
            PhaseSpec::new("decode", 1, Total::Exact(rows)),
            PhaseSpec::new("tiles", 1, Total::Unknown),
        ],
    )
    .complete_with(|stages| {
        stages.run(|stage| {
            for _ in 0..rows {
                stage.step(1)?;
            }
            Ok(())
        })?;
        stages.run(|stage| {
            let tiles = stage.plan(
                Execution::ForkJoin,
                &[
                    PhaseSpec::new("left", 1, Total::Exact(1)),
                    PhaseSpec::new("right", 1, Total::Exact(1)),
                ],
            );
            let mut done = 0;
            for tile in tiles {
                done += 1;
                tile.complete_with(|tile| tile.step(1))?;
            }
            Ok(rows + done)
        })
    })
}

#[test]
fn every_nested_phase_checks_the_one_token() {
    let token = Token::default();
    assert_eq!(library(4, &StopOnly::new(token.clone())), Ok(6));
    // Entry, four rows and two tiles all reached the application's token.
    assert!(token.checks.load(Ordering::Relaxed) >= 6);
    token.cancelled.store(true, Ordering::Relaxed);
    assert_eq!(
        library(4, &StopOnly::borrowed(&token)),
        Err(StopReason::Cancelled)
    );
}

#[test]
fn reports_and_totals_are_accepted_and_discarded() {
    let pulse = StopOnly::borrowed(&how_far::Unstoppable);
    assert!(!pulse.may_report());
    assert!(!pulse.may_stop());
    assert!(pulse.live().is_none(), "nothing to call in a hot loop");
    pulse.advance(10);
    assert_eq!(pulse.set_total(Total::Exact(3)), Ok(()));
    assert!(StopOnly::new(Token::default()).may_stop());
}

#[test]
fn plans_are_validated_like_any_pulse() {
    let pulse = StopOnly::new(Token::default());
    assert_eq!(
        pulse.split(Execution::Sequence, &[]).err(),
        NoPulse.split(Execution::Sequence, &[]).err()
    );
    assert_eq!(
        pulse
            .split(
                Execution::Sequence,
                &[PhaseSpec::new("zero", 0, Total::Unknown)]
            )
            .err(),
        Some(PlanError::EmptyOrZeroWeight)
    );
    let [child] = pulse
        .split_array(
            Execution::Sequence,
            [PhaseSpec::new("a", 1, Total::Unknown)],
        )
        .unwrap();
    child.finish(Outcome::Succeeded).unwrap();
}

#[test]
fn an_owned_token_reaches_spawned_work_through_every_level() {
    let token = Token::default();
    let pulse = StopOnly::new(token.clone());
    let [child] = pulse
        .split_array(
            Execution::Sequence,
            [PhaseSpec::new("a", 1, Total::Unknown)],
        )
        .unwrap();
    // A child of an owned token can still be shared with `'static` work.
    let shared = child.share().unwrap();
    let worker = std::thread::spawn(move || {
        while shared.check().is_ok() {
            std::thread::yield_now();
        }
        shared.check()
    });
    token.cancelled.store(true, Ordering::Relaxed);
    assert_eq!(worker.join().unwrap(), Err(StopReason::Cancelled));
    child.finish(Outcome::Cancelled).unwrap();
}

#[test]
fn a_borrowed_token_refuses_to_be_shared_at_any_level() {
    let token = Token::default();
    let pulse = StopOnly::borrowed(&token);
    assert_eq!(pulse.share().err(), Some(PlanError::NotShareable));
    let [child] = pulse
        .split_array(
            Execution::Sequence,
            [PhaseSpec::new("a", 1, Total::Unknown)],
        )
        .unwrap();
    assert_eq!(child.share().err(), Some(PlanError::NotShareable));
    // Scoped workers borrow it instead.
    std::thread::scope(|scope| {
        scope.spawn(|| child.check().unwrap());
    });
    child.finish(Outcome::Succeeded).unwrap();
}

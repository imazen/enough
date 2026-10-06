//! Observer-side interpolation between reports.
#![cfg(feature = "interpolate")]

use how_far_along::interpolate::Interpolator;
use how_far_along::{Complete, Execution, Observer, Outcome, Phase, PhaseSpec, Report, Total};
use std::time::Duration;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn at(smooth: &mut Interpolator, observer: &Observer, now: u64) -> f64 {
    smooth.fraction(&observer.snapshot(), ms(now)).unwrap()
}

fn near(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
}

#[test]
fn evenly_spaced_reports_are_extrapolated_up_to_the_next_one() {
    let mut job = Phase::new("rows", Total::Exact(100));
    job.start().unwrap();
    let (reporter, observer) = (job.reporter(), job.observer());
    let mut smooth = Interpolator::new();
    near(at(&mut smooth, &observer, 0), 0.0);
    // Ten rows every 50 ms.
    reporter.advance(10);
    near(at(&mut smooth, &observer, 50), 0.10);
    near(at(&mut smooth, &observer, 60), 0.12);
    near(at(&mut smooth, &observer, 75), 0.15);
    reporter.advance(10);
    near(at(&mut smooth, &observer, 100), 0.20);
    near(at(&mut smooth, &observer, 140), 0.28);
    // A late report holds the display at the next expected count.
    near(at(&mut smooth, &observer, 400), 0.30);
    reporter.advance(10);
    near(at(&mut smooth, &observer, 410), 0.30);
}

#[test]
fn a_smaller_report_than_expected_never_moves_the_display_back() {
    let mut job = Phase::new("tiles", Total::Exact(100));
    job.start().unwrap();
    let (reporter, observer) = (job.reporter(), job.observer());
    let mut smooth = Interpolator::new();
    at(&mut smooth, &observer, 0);
    reporter.advance(10);
    at(&mut smooth, &observer, 100);
    near(at(&mut smooth, &observer, 190), 0.19);
    // Only 5 more units arrive; the display holds at 0.19 until work passes it.
    reporter.advance(5);
    near(at(&mut smooth, &observer, 200), 0.19);
    reporter.advance(10);
    assert!(at(&mut smooth, &observer, 210) >= 0.25);
}

#[test]
fn the_estimate_never_passes_the_total_or_runs_before_a_pace_is_known() {
    let mut job = Phase::new("short", Total::Exact(3));
    job.start().unwrap();
    let (reporter, observer) = (job.reporter(), job.observer());
    let mut smooth = Interpolator::new();
    // Attached after work began: no change seen yet, so no pace.
    reporter.advance(2);
    near(at(&mut smooth, &observer, 0), 2.0 / 3.0);
    near(at(&mut smooth, &observer, 5_000), 2.0 / 3.0);
    reporter.advance(1);
    near(at(&mut smooth, &observer, 5_010), 1.0);
    near(at(&mut smooth, &observer, 9_000), 1.0);
}

#[test]
fn each_stage_keeps_its_own_pace_within_the_weighted_tree() {
    let mut job = Phase::new("convert", Total::Unknown);
    let mut stages = job
        .split_vec(
            Execution::Sequence,
            &[
                PhaseSpec::new("decode", 1, Total::Exact(10)),
                PhaseSpec::new("encode", 3, Total::Exact(10)),
            ],
        )
        .unwrap();
    let observer = job.observer();
    let mut smooth = Interpolator::new();
    let (decode, encode) = (stages[0].reporter(), stages[1].reporter());
    stages[0].start().unwrap();
    at(&mut smooth, &observer, 0);
    // Decode: one unit every 10 ms.
    for i in 1..=10 {
        decode.advance(1);
        at(&mut smooth, &observer, 10 * i);
    }
    stages[0].finish().unwrap();
    near(at(&mut smooth, &observer, 100), 0.25);
    // Encode starts slower: one unit every 100 ms. The fast decode pace is
    // not carried over: until encode has changed once, it shows its count.
    stages[1].start().unwrap();
    near(at(&mut smooth, &observer, 110), 0.25);
    encode.advance(1);
    near(at(&mut smooth, &observer, 210), 0.25 + 0.75 * 0.1);
    near(at(&mut smooth, &observer, 260), 0.25 + 0.75 * 0.15);
    encode.advance(9);
    stages[1].finish().unwrap();
    job.finish().unwrap();
    near(at(&mut smooth, &observer, 300), 1.0);
}

#[test]
fn unknown_totals_are_none_and_finished_leaves_are_forgotten() {
    let job = Phase::new("unknown", Total::Unknown);
    let mut smooth = Interpolator::new();
    job.reporter().advance(5);
    assert_eq!(smooth.fraction(&job.observer().snapshot(), ms(0)), None);

    let mut job = Phase::new("known", Total::Exact(4));
    job.start().unwrap();
    let observer = job.observer();
    job.reporter().advance(1);
    at(&mut smooth, &observer, 0);
    job.reporter().advance(1);
    at(&mut smooth, &observer, 10);
    job.complete_as(Outcome::Succeeded);
    near(at(&mut smooth, &observer, 20), 1.0);
    // The finished leaf's pace was dropped; the display stays at 1.
    assert!(format!("{smooth:?}").contains("running_leaves: 0"));
}

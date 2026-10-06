//! OS threads: scoped and spawned workers, observers on other threads, and
//! cancellation that crosses threads.

use almost_enough::Stopper;
use how_far_along::{IsStop, Outcome, ProgressExt, Pulse, Status, Unstoppable};
use std::{sync::Arc, thread, time::Duration};
mod common;
use common::{all, find, images, tree, tree_stopping_when};
use how_far_example_codec::{self as codec, Image, encode_detached, encode_groups};
use how_far_example_pipeline::process_each;

#[test]
fn scoped_fork_join_children_each_finish_with_their_own_counts() {
    let image = Image::pattern(32, 16 * 10);
    let tracked = tree("groups", Unstoppable);
    let observer = tracked.observer();
    let bytes = encode_groups(&image, 4, &tracked).unwrap();
    assert_eq!(bytes.len(), image.pixels.len());
    tracked.finish(Outcome::Succeeded).unwrap();
    let root = observer.snapshot();
    assert_eq!(root.children.len(), 4);
    assert_eq!(
        root.children
            .iter()
            .map(|group| group.completed)
            .sum::<u64>(),
        image.tiles() as u64
    );
    assert!(
        all(&root)
            .iter()
            .all(|phase| phase.status == Status::Finished(Outcome::Succeeded))
    );
    assert_eq!(root.fraction(), Some(1.0));
}

#[test]
fn spawned_static_threads_count_through_owned_handles() {
    let image = Image::pattern(16, 16 * 12);
    let tracked = tree("detached", Unstoppable);
    let observer = tracked.observer();
    let bytes = encode_detached(&image, &tracked).unwrap();
    assert_eq!(bytes.len(), image.pixels.len());
    assert_eq!(observer.snapshot().completed, image.tiles() as u64);
    tracked.finish(Outcome::Succeeded).unwrap();
}

#[test]
fn a_stop_tripped_on_one_worker_stops_its_siblings() {
    let image = Image::pattern(8, 16 * 64);
    let tracked = tree_stopping_when("groups", |root| {
        root.children
            .iter()
            .map(|group| group.completed)
            .sum::<u64>()
            >= 8
    });
    let observer = tracked.observer();
    let error = encode_groups(&image, 4, &tracked).unwrap_err();
    assert_eq!(
        error,
        codec::Error::Stopped(how_far_along::StopReason::Cancelled)
    );
    tracked.finish(Outcome::Cancelled).unwrap();
    let root = observer.snapshot();
    let done: u64 = root.children.iter().map(|group| group.completed).sum();
    // Each worker may finish the tile it is on before its next check.
    assert!((8..8 + 4).contains(&done), "{done}");
    assert!(
        root.children
            .iter()
            .all(|group| matches!(group.status, Status::Finished(_)))
    );
}

#[test]
fn an_observer_on_another_thread_sees_monotonic_progress() {
    // Every stage has an exact total from the start, so every sample has a
    // fraction; counters only grow, so the fraction never goes down.
    let image = Image::pattern(64, 16 * 400);
    let tracked = tree("encode", Unstoppable);
    let observer = tracked.observer();
    let samples = thread::scope(|scope| {
        let sampler = scope.spawn(|| {
            let mut fractions = Vec::new();
            while !observer.is_finished() {
                if let Some(fraction) = observer.try_snapshot().and_then(|s| s.fraction()) {
                    fractions.push(fraction);
                }
                thread::yield_now();
            }
            fractions.push(observer.snapshot().fraction().unwrap());
            fractions
        });
        let result = how_far_example_codec::encode(&image, &tracked);
        tracked
            .finish(Outcome::from_result(&result, |error| {
                error.stop_reason().is_some()
            }))
            .unwrap();
        sampler.join().unwrap()
    });
    assert!(
        samples.windows(2).all(|pair| pair[0] <= pair[1]),
        "{samples:?}"
    );
    assert_eq!(samples.last(), Some(&1.0));
}

#[test]
fn a_tree_moves_into_a_spawned_thread_and_is_observed_from_here() {
    let batch = Arc::new(images(3));
    let tracked = tree("batch", Unstoppable);
    let observer = tracked.observer();
    let work = Arc::clone(&batch);
    let worker = thread::spawn(move || {
        let result = process_each(&work, &tracked);
        tracked
            .finish(Outcome::from_result(&result, |error| {
                error.stop_reason().is_some()
            }))
            .unwrap();
        result.map(|encoded| encoded.len())
    });
    assert_eq!(worker.join().unwrap(), Ok(3));
    let root = observer.snapshot();
    assert_eq!(root.status, Status::Finished(Outcome::Succeeded));
    assert_eq!(
        find(&root, &["image 2", "analyze"]).completed,
        batch[2].height as u64
    );
}

#[test]
fn cancel_from_the_main_thread_reaches_workers_on_other_threads() {
    // Rows are cheap, so a 4M-row analysis gives the cancel time to land.
    let image = Image::pattern(1, 4_000_000);
    let stop = Stopper::new();
    let tracked = tree("long", stop.clone());
    let observer = tracked.observer();
    let worker = thread::spawn(move || {
        let result = how_far_example_codec::encode(&image, &tracked);
        tracked
            .finish(Outcome::from_result(&result, |error| {
                error.stop_reason().is_some()
            }))
            .unwrap();
        result.map(|bytes| bytes.len())
    });
    while observer.try_snapshot().is_none_or(|root| {
        root.children
            .first()
            .is_none_or(|analyze| analyze.completed == 0)
    }) {
        thread::sleep(Duration::from_micros(50));
    }
    stop.cancel();
    assert_eq!(
        worker.join().unwrap(),
        Err(codec::Error::Stopped(how_far_along::StopReason::Cancelled))
    );
    let root = observer.snapshot();
    assert_eq!(root.status, Status::Finished(Outcome::Cancelled));
    assert!(root.children[0].completed < 4_000_000);
}

#[test]
fn a_shared_pulse_reference_works_from_many_scoped_threads() {
    let tracked = tree("shared", Unstoppable);
    let observer = tracked.observer();
    let pulse: &dyn Pulse = &tracked;
    thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(move || {
                for _ in 0..1_000 {
                    pulse.step(1).unwrap();
                }
            });
        }
    });
    assert_eq!(observer.snapshot().completed, 8_000);
    tracked.finish(Outcome::Succeeded).unwrap();
}

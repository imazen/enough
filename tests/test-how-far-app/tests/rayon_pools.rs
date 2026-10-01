//! Rayon: shared stages, fork-join children, recursive `join`, nested
//! parallelism, and `'static` tasks.

use how_far_along::{Outcome, ProgressExt, Pulse, Status, Unstoppable};
use std::sync::mpsc;
use test_how_far_app::{all, find, images, tree};
use test_how_far_codec::{CodecError, Image, encode, encode_quadtree};
use test_how_far_pipeline::{PipelineError, process};

fn pool(threads: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap()
}

#[test]
fn a_stage_shared_by_rayon_workers_counts_every_tile_once() {
    let image = Image::pattern(48, 16 * 33 + 5);
    for threads in [1, 2, 4, 8] {
        let tracked = tree("encode", Unstoppable);
        let observer = tracked.observer();
        let bytes = pool(threads).install(|| encode(&image, &tracked)).unwrap();
        assert!(!bytes.is_empty());
        tracked.finish(Outcome::Succeeded).unwrap();
        let root = observer.snapshot();
        let transform = find(&root, &["transform"]);
        assert_eq!(
            transform.completed,
            image.tiles() as u64,
            "{threads} threads"
        );
        assert!(transform.children.is_empty(), "no child per worker");
    }
}

#[test]
fn nested_parallelism_records_a_child_per_image_with_its_own_stages() {
    let batch = images(6);
    let tracked = tree("batch", Unstoppable);
    let observer = tracked.observer();
    // Images run in parallel, and each image's transform runs in parallel too.
    let encoded = pool(4).install(|| process(&batch, &tracked)).unwrap();
    assert_eq!(encoded.len(), batch.len());
    tracked.finish(Outcome::Succeeded).unwrap();
    let root = observer.snapshot();
    assert_eq!(find(&root, &["encode"]).children.len(), 6);
    assert!(
        all(&root)
            .iter()
            .all(|phase| phase.status == Status::Finished(Outcome::Succeeded))
    );
    assert_eq!(root.fraction(), Some(1.0));
}

#[test]
fn recursive_rayon_join_plans_a_child_at_every_level() {
    let image = Image::pattern(16, 16 * 37);
    let tracked = tree("quadtree", Unstoppable);
    let observer = tracked.observer();
    let total = pool(4)
        .install(|| encode_quadtree(&image, &tracked))
        .unwrap();
    assert_eq!(total, image.pixels.len() as u64);
    tracked.finish(Outcome::Succeeded).unwrap();
    let root = observer.snapshot();
    let phases = all(&root);
    let leaf_count: u64 = phases
        .iter()
        .filter(|phase| phase.children.is_empty())
        .map(|phase| phase.completed)
        .sum();
    assert_eq!(leaf_count, image.tiles() as u64);
    assert!(phases.len() > 20, "{} phases", phases.len());
    assert!(
        phases
            .iter()
            .all(|phase| phase.status == Status::Finished(Outcome::Succeeded))
    );
    assert_eq!(root.fraction(), Some(1.0));
}

#[test]
fn the_global_pool_works_like_a_custom_one() {
    let batch = images(3);
    let tracked = tree("batch", Unstoppable);
    let observer = tracked.observer();
    let result = process(&batch, &tracked);
    tracked
        .finish(Outcome::from_result(&result, PipelineError::is_stop))
        .unwrap();
    assert_eq!(observer.snapshot().fraction(), Some(1.0));
}

#[test]
fn static_rayon_spawns_report_through_handles() {
    let tracked = tree("spawned", Unstoppable);
    let observer = tracked.observer();
    let (done, finished) = mpsc::channel();
    for _ in 0..16 {
        let handle = tracked.handle();
        let done = done.clone();
        // `rayon::spawn` needs `'static`, so it cannot borrow the pulse.
        rayon::spawn(move || {
            for _ in 0..4 {
                handle.step(1).unwrap();
            }
            done.send(()).unwrap();
        });
    }
    for _ in 0..16 {
        finished.recv().unwrap();
    }
    assert_eq!(observer.snapshot().completed, 64);
    tracked.finish(Outcome::Succeeded).unwrap();
}

#[test]
fn a_rayon_scope_can_borrow_a_pulse_and_its_children() {
    let tracked = tree("scope", Unstoppable);
    let observer = tracked.observer();
    let children = tracked
        .split(
            how_far_along::Execution::ForkJoin,
            &[
                how_far_along::PhaseSpec::new("a", 1, how_far_along::Total::Exact(10)),
                how_far_along::PhaseSpec::new("b", 1, how_far_along::Total::Exact(10)),
            ],
        )
        .unwrap();
    rayon::scope(|scope| {
        for child in children {
            scope.spawn(move |_| {
                for _ in 0..10 {
                    child.step(1).unwrap();
                }
                child.finish(Outcome::Succeeded).unwrap();
            });
        }
    });
    tracked.finish(Outcome::Succeeded).unwrap();
    assert_eq!(observer.snapshot().fraction(), Some(1.0));
}

#[test]
fn a_codec_failure_on_one_rayon_image_leaves_the_others_finished() {
    let mut batch = images(4);
    batch[2] = batch[2].clone().damaged_at(3);
    let tracked = tree("batch", Unstoppable);
    let observer = tracked.observer();
    let result = pool(4).install(|| process(&batch, &tracked));
    assert_eq!(
        result,
        Err(PipelineError::Codec {
            image: 2,
            error: CodecError::Corrupt { row: 3 }
        })
    );
    tracked
        .finish(Outcome::from_result(&result, PipelineError::is_stop))
        .unwrap();
    let root = observer.snapshot();
    assert_eq!(root.status, Status::Finished(Outcome::Failed));
    let encode = find(&root, &["encode"]);
    assert_eq!(encode.status, Status::Finished(Outcome::Failed));
    assert_eq!(
        find(encode, &["image 2"]).status,
        Status::Finished(Outcome::Failed)
    );
    for other in ["image 0", "image 1", "image 3"] {
        assert_eq!(
            find(encode, &[other]).status,
            Status::Finished(Outcome::Succeeded)
        );
    }
}

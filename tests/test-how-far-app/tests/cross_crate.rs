//! An application drives a pipeline library that calls a codec library, both
//! in other crates, through one `&dyn Pulse`.

use how_far_along::{NoPulse, Outcome, Status};
use test_how_far_app::{all, find, images, tree, tree_stopping_when};
use test_how_far_codec::{CodecError, Image};
use test_how_far_pipeline::{PipelineError, process, process_each};

#[test]
fn tracking_never_changes_the_output() {
    let batch = images(3);
    let tracked = tree("batch", how_far_along::Unstoppable);
    assert_eq!(
        process(&batch, &tracked).unwrap(),
        process(&batch, &NoPulse).unwrap()
    );
    assert_eq!(
        process_each(&batch, &tracked_tree()).unwrap(),
        process_each(&batch, &NoPulse).unwrap()
    );
}

fn tracked_tree() -> how_far_along::PulseTree {
    tree("batch", how_far_along::Unstoppable)
}

#[test]
fn one_tree_records_both_libraries_phases() {
    let batch = images(3);
    let tracked = tree("batch", how_far_along::Unstoppable);
    let observer = tracked.observer();
    let result = process(&batch, &tracked);
    tracked
        .finish(Outcome::from_result(&result, PipelineError::is_stop))
        .unwrap();
    let root = observer.snapshot();
    assert_eq!(root.status, Status::Finished(Outcome::Succeeded));
    assert_eq!(root.fraction(), Some(1.0));
    for (i, image) in batch.iter().enumerate() {
        let name = format!("image {i}");
        let analyze = find(&root, &["encode", &name, "analyze"]);
        let transform = find(&root, &["encode", &name, "transform"]);
        let entropy = find(&root, &["encode", &name, "entropy"]);
        assert_eq!(analyze.completed, image.height as u64);
        assert_eq!(analyze.units, "rows");
        assert_eq!(transform.completed, image.tiles() as u64);
        assert_eq!(entropy.completed, image.tiles() as u64);
    }
    for phase in all(&root) {
        assert_eq!(
            phase.status,
            Status::Finished(Outcome::Succeeded),
            "{}",
            phase.name
        );
    }
}

#[test]
fn a_library_runs_the_codec_inside_each_of_its_stages() {
    let batch = images(2);
    let tracked = tree("batch", how_far_along::Unstoppable);
    let observer = tracked.observer();
    let result = process_each(&batch, &tracked);
    assert!(result.is_ok());
    // Neither library finished the root; the application does.
    assert_eq!(observer.snapshot().status, Status::Running);
    tracked.finish(Outcome::Succeeded).unwrap();
    let root = observer.snapshot();
    assert_eq!(root.fraction(), Some(1.0));
    assert_eq!(
        find(&root, &["image 1", "entropy"]).status,
        Status::Finished(Outcome::Succeeded)
    );
}

#[test]
fn cancelling_inside_the_codec_records_each_level_and_returns_the_stop() {
    let batch = images(3);
    let tracked = tree_stopping_when("batch", |root| {
        root.children.len() > 1
            && root.children[1].children.len() == 3
            && root.children[1].children[0].completed >= 5
    });
    let observer = tracked.observer();
    let result = process_each(&batch, &tracked);
    assert_eq!(
        result,
        Err(PipelineError::Codec {
            image: 1,
            error: CodecError::Stopped(how_far_along::StopReason::Cancelled)
        })
    );
    tracked
        .finish(Outcome::from_result(&result, PipelineError::is_stop))
        .unwrap();
    let root = observer.snapshot();
    assert_eq!(root.status, Status::Finished(Outcome::Cancelled));
    let image = |i: usize| format!("image {i}");
    let status = |path: &[&str]| find(&root, path).status;
    assert_eq!(status(&[&image(0)]), Status::Finished(Outcome::Succeeded));
    assert_eq!(status(&[&image(1)]), Status::Finished(Outcome::Cancelled));
    assert_eq!(
        status(&[&image(1), "analyze"]),
        Status::Finished(Outcome::Cancelled)
    );
    assert_eq!(find(&root, &[&image(1), "analyze"]).completed, 5);
    assert_eq!(
        status(&[&image(1), "transform"]),
        Status::Finished(Outcome::NotRun)
    );
    assert_eq!(status(&[&image(2)]), Status::Finished(Outcome::NotRun));
}

#[test]
fn corrupt_input_is_recorded_as_a_failure_not_a_cancellation() {
    let mut batch = images(3);
    batch[1] = batch[1].clone().damaged_at(7);
    let tracked = tree("batch", how_far_along::Unstoppable);
    let observer = tracked.observer();
    let result = process_each(&batch, &tracked);
    assert_eq!(
        result,
        Err(PipelineError::Codec {
            image: 1,
            error: CodecError::Corrupt { row: 7 }
        })
    );
    tracked
        .finish(Outcome::from_result(&result, PipelineError::is_stop))
        .unwrap();
    let root = observer.snapshot();
    assert_eq!(root.status, Status::Finished(Outcome::Failed));
    assert_eq!(
        find(&root, &["image 1", "analyze"]).status,
        Status::Finished(Outcome::Failed)
    );
    assert_eq!(
        find(&root, &["image 2"]).status,
        Status::Finished(Outcome::NotRun)
    );
}

#[test]
fn parallel_images_each_get_an_outcome_when_one_is_cancelled() {
    let batch = images(4);
    let tracked = tree_stopping_when("batch", |root| {
        test_how_far_app::all(root)
            .iter()
            .filter(|phase| phase.name == "transform")
            .map(|phase| phase.completed)
            .sum::<u64>()
            >= 3
    });
    let observer = tracked.observer();
    let result = process(&batch, &tracked);
    let error = result.unwrap_err();
    assert!(error.is_stop(), "{error:?}");
    tracked.finish(Outcome::Cancelled).unwrap();
    let root = observer.snapshot();
    let encode = find(&root, &["encode"]);
    assert_eq!(encode.status, Status::Finished(Outcome::Cancelled));
    for image in &encode.children {
        assert!(
            matches!(
                image.status,
                Status::Finished(Outcome::Succeeded | Outcome::Cancelled)
            ),
            "{}: {:?}",
            image.name,
            image.status
        );
    }
    assert!(
        encode
            .children
            .iter()
            .any(|image| image.status == Status::Finished(Outcome::Cancelled))
    );
    assert_eq!(
        find(&root, &["pack"]).status,
        Status::Finished(Outcome::NotRun)
    );
}

#[test]
fn an_empty_image_still_succeeds() {
    let empty = Image::pattern(8, 0);
    let tracked = tree("empty", how_far_along::Unstoppable);
    let observer = tracked.observer();
    test_how_far_codec::encode(&empty, &tracked).unwrap();
    tracked.finish(Outcome::Succeeded).unwrap();
    assert_eq!(observer.snapshot().fraction(), Some(1.0));
}

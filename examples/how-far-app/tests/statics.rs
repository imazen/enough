//! `'static` ownership: trees moved between threads, shared views stored in
//! codec contexts, and async runtimes.

use almost_enough::Stopper;
use how_far::{SharedPulse, Stages};
use how_far_along::{
    Child, IsStop, Observer, Outcome, Phase, ProgressExt, Pulse, PulseTree, Reporter, Snapshot,
    Status, Stop, StopReason, Unstoppable,
};
use std::sync::Arc;
mod common;
use common::{find, tree, tree_stopping_when};
use how_far_example_codec::{self as codec, EntropyCoder, Image, encode};

fn owned<T: Send + Sync + 'static>() {}
fn shared<T: Send + Sync>() {}

#[test]
fn shared_views_and_trees_are_owned_and_thread_safe() {
    owned::<PulseTree>();
    owned::<SharedPulse>();
    owned::<Phase>();
    owned::<Reporter>();
    owned::<Observer>();
    owned::<Snapshot>();
    owned::<Arc<dyn Pulse>>();
    owned::<Box<dyn Pulse>>();
    shared::<Child<'_>>();
    shared::<Stages<'_>>();
    shared::<&dyn Pulse>();
}

#[test]
fn a_codec_context_that_owns_its_stop_is_cancelled_mid_loop() {
    // Many codecs keep `impl Stop + 'static` in a context object. A shared
    // view reaches inside it: the coder stops between tiles, not after the frame.
    let tiles: Vec<Vec<u8>> = (0..12).map(|i| vec![i as u8; 256]).collect();
    let tracked = tree_stopping_when("coder", |root| root.completed >= 3);
    let observer = tracked.observer();
    let mut coder = EntropyCoder::new(tracked.share().unwrap());
    assert_eq!(
        coder.code(&tiles),
        Err(codec::Error::Stopped(StopReason::Cancelled))
    );
    assert_eq!(observer.snapshot().completed, 3);
    tracked.finish(Outcome::Cancelled).unwrap();
}

#[test]
fn a_stage_view_stored_by_the_codec_stops_inside_its_stage() {
    let image = Image::pattern(32, 16 * 20);
    let tracked = tree_stopping_when("encode", |root| {
        root.children.len() == 3 && root.children[2].completed >= 4
    });
    let observer = tracked.observer();
    let result = encode(&image, &tracked);
    assert_eq!(result, Err(codec::Error::Stopped(StopReason::Cancelled)));
    tracked
        .finish(Outcome::from_result(&result, |error| {
            error.stop_reason().is_some()
        }))
        .unwrap();
    let root = observer.snapshot();
    let entropy = find(&root, &["entropy"]);
    assert_eq!(entropy.status, Status::Finished(Outcome::Cancelled));
    assert_eq!(entropy.completed, 4);
}

#[test]
fn a_shared_arc_pulse_serves_many_threads_and_the_tree_is_recovered_to_finish() {
    let tracked = Arc::new(tree("arc", Unstoppable));
    let observer = tracked.observer();
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let pulse = Arc::clone(&tracked);
            std::thread::spawn(move || {
                for _ in 0..25 {
                    pulse.step(1).unwrap();
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let tracked = Arc::try_unwrap(tracked).expect("every worker joined");
    tracked.finish(Outcome::Succeeded).unwrap();
    assert_eq!(observer.snapshot().completed, 100);
}

#[test]
fn a_boxed_pulse_can_be_stored_and_passed_on() {
    struct Job {
        pulse: Box<dyn Pulse>,
    }
    let job = Job {
        pulse: Box::new(tree("boxed", Unstoppable)),
    };
    let image = Image::pattern(16, 32);
    encode(&image, job.pulse.as_ref()).unwrap();
}

#[test]
fn blocking_work_runs_on_tokio_while_an_async_task_watches_and_cancels() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .unwrap();
    runtime.block_on(async {
        let image = Image::pattern(1, 3_000_000);
        let stop = Stopper::new();
        let tracked = tree("request", stop.clone());
        let observer = tracked.observer();
        let work = tokio::task::spawn_blocking(move || {
            let result = encode(&image, &tracked);
            tracked
                .finish(Outcome::from_result(&result, |error| {
                    error.stop_reason().is_some()
                }))
                .unwrap();
            result
        });
        // The async side samples without blocking and cancels once work started.
        loop {
            let started = observer
                .try_snapshot()
                .and_then(|root| root.children.first().map(|analyze| analyze.completed > 0))
                .unwrap_or(false);
            if started {
                stop.cancel();
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            work.await.unwrap(),
            Err(codec::Error::Stopped(StopReason::Cancelled))
        );
        assert_eq!(
            observer.snapshot().status,
            Status::Finished(Outcome::Cancelled)
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_tasks_report_through_shared_views() {
    let tracked = tree("async", Unstoppable);
    let observer = tracked.observer();
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let shared = tracked.share().unwrap();
            tokio::spawn(async move {
                for _ in 0..5 {
                    shared.step(1)?;
                    tokio::task::yield_now().await;
                }
                Ok::<_, StopReason>(())
            })
        })
        .collect();
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    assert_eq!(observer.snapshot().completed, 40);
    tracked.finish(Outcome::Succeeded).unwrap();
}

#[test]
fn a_shared_view_outlives_its_tree_harmlessly() {
    let tracked = tree("short-lived", Unstoppable);
    let observer = tracked.observer();
    let shared = tracked.share().unwrap();
    tracked.finish(Outcome::Succeeded).unwrap();
    // Late reports from a stale view cannot change a finished record.
    shared.step(10).unwrap();
    assert_eq!(observer.snapshot().completed, 0);
    assert!(shared.check().is_ok());
}

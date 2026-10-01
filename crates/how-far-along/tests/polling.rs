//! Callback dispatch over snapshots, on the owner thread and on workers.

use almost_enough::Stopper;
use how_far_along::poll::{LocalPoller, SharedPoller};
use how_far_along::{
    NoReport, Outcome, Phase, ProgressExt, ProgressWithStop, Report, Status, Stop, StopReason,
    Total,
};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
};

#[test]
fn local_callbacks_are_thread_affine_mutable_lazy_and_memoized() {
    let mut job = Phase::new("local", Total::Exact(10));
    let reporter = job.reporter();
    let saved = Rc::new(RefCell::new(None));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let owner = std::thread::current().id();
    let mut poller = LocalPoller::new(job.observer());
    let seen_first = seen.clone();
    poller.subscribe(move |event| {
        assert_eq!(std::thread::current().id(), owner);
        assert!(!event.snapshot_materialized());
        seen_first.borrow_mut().push("work without a snapshot");
    });
    let saved_first = saved.clone();
    poller.subscribe(move |event| {
        assert!(event.try_snapshot().is_some());
        *saved_first.borrow_mut() = Some(event.snapshot_owned());
        reporter.advance(1); // Reports after the snapshot must not change this dispatch.
    });
    let saved_second = saved.clone();
    poller.subscribe(move |event| {
        assert!(event.snapshot_materialized());
        assert!(Arc::ptr_eq(
            saved_second.borrow().as_ref().unwrap(),
            &event.snapshot_owned()
        ));
        assert_eq!(
            event.snapshot().completed + 1,
            event.observer().snapshot().completed
        );
    });
    poller.poll();
    assert_eq!(saved.borrow().as_ref().unwrap().completed, 0);
    assert_eq!(seen.borrow().len(), 1);
    job.finish().unwrap();
}

#[test]
fn subscribers_run_in_order_and_a_final_poll_delivers_the_outcome() {
    let mut job = Phase::new("job", Total::Exact(4));
    let stop = Stopper::new();
    let mut poller = LocalPoller::new(job.observer());
    let cancel = stop.clone();
    let first = poller.subscribe(move |_| cancel.cancel());
    let later = stop.clone();
    poller.subscribe(move |_| assert!(later.check().is_err()));
    poller.poll();
    assert_eq!(stop.check(), Err(StopReason::Cancelled));
    assert!(poller.unsubscribe(first));
    assert!(!poller.unsubscribe(first));
    job.finish_with(Outcome::Cancelled).unwrap();
    let saved = Rc::new(RefCell::new(None));
    let copy = saved.clone();
    poller.subscribe(move |event| *copy.borrow_mut() = Some(event.snapshot_owned()));
    poller.poll(); // Deliver the final state regardless of any timer.
    assert_eq!(
        saved.borrow().as_ref().unwrap().status,
        Status::Finished(Outcome::Cancelled)
    );
}

#[test]
fn a_busy_shared_poll_returns_at_once_and_cancellation_does_not_wait_for_it() {
    let job = Phase::new("job", Total::Unknown);
    let stop = Stopper::new();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (exit_tx, exit_rx) = mpsc::channel();
    let exit_rx = Mutex::new(exit_rx);
    let poller = SharedPoller::new(job.observer(), move |_| {
        entered_tx.send(()).unwrap();
        exit_rx.lock().unwrap().recv().unwrap();
    });
    std::thread::scope(|scope| {
        scope.spawn(|| assert!(poller.try_poll()));
        entered_rx.recv().unwrap();
        stop.cancel();
        assert!(!poller.try_poll(), "busy");
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
        exit_tx.send(()).unwrap();
    });
}

#[test]
fn a_recursive_poll_is_busy_and_a_panic_releases_the_claim() {
    let job = Phase::new("job", Total::Unknown);
    let slot = Arc::new(Mutex::new(None::<SharedPoller>));
    let slot_in = slot.clone();
    let panic_once = AtomicBool::new(true);
    let poller = SharedPoller::new(job.observer(), move |_| {
        let nested = slot_in.lock().unwrap().as_ref().unwrap().clone();
        assert!(!nested.try_poll());
        if panic_once.swap(false, Ordering::Relaxed) {
            panic!("subscriber panicked");
        }
    });
    *slot.lock().unwrap() = Some(poller.clone());
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| poller.try_poll())).is_err());
    assert!(poller.try_poll());
    slot.lock().unwrap().take();
}

#[test]
fn shared_callbacks_never_overlap_and_share_one_lazy_snapshot() {
    let job = Phase::new("job", Total::Unknown);
    let active = Arc::new(AtomicUsize::new(0));
    let count = Arc::new(AtomicUsize::new(0));
    let mut builder = SharedPoller::builder(job.observer());
    let a = active.clone();
    let c = count.clone();
    builder.subscribe(move |event| {
        assert_eq!(a.fetch_add(1, Ordering::SeqCst), 0);
        assert!(!event.snapshot_materialized());
        event.snapshot();
        c.fetch_add(1, Ordering::Relaxed);
        a.fetch_sub(1, Ordering::SeqCst);
    });
    builder.subscribe(|event| assert!(event.snapshot_materialized()));
    let poller = builder.build();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..100 {
                    poller.try_poll();
                }
            });
        }
    });
    assert!(count.load(Ordering::Relaxed) > 0);
}

#[test]
fn workers_poll_at_checkpoints_and_a_callback_can_stop_them() {
    let mut job = Phase::new("blocks", Total::Exact(1_000));
    let stop = Stopper::new();
    let cancel = stop.clone();
    let poller = SharedPoller::new(job.observer(), move |event| {
        if event.snapshot().completed >= 100 {
            cancel.cancel();
        }
    });
    let reporter = job.reporter();
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let work = ProgressWithStop::new(stop.clone(), reporter.clone());
            let poller = poller.clone();
            scope.spawn(move || {
                for _ in 0..250 {
                    if work.step(1).is_err() {
                        return;
                    }
                    poller.try_poll();
                }
            });
        }
    });
    assert_eq!(stop.check(), Err(StopReason::Cancelled));
    let completed = job.observer().snapshot().completed;
    assert!((100..1_000).contains(&completed), "{completed}");
    job.finish_with(Outcome::Cancelled).unwrap();
}

#[test]
fn posted_delivery_moves_snapshots_to_the_owner_thread() {
    let mut job = Phase::new("posted", Total::Exact(1));
    let reporter = job.reporter();
    let (tx, rx) = mpsc::sync_channel(1);
    let poller = SharedPoller::new(job.observer(), move |event| {
        // The consumer chooses bounded, coalescing delivery.
        let _ = tx.try_send(event.snapshot_owned());
    });
    std::thread::scope(|scope| {
        scope.spawn(move || {
            reporter.advance(1);
            poller.try_poll();
        });
        assert_eq!(rx.recv().unwrap().completed, 1);
    });
    job.finish().unwrap();
}

#[test]
fn a_no_op_pair_compiles_away() {
    let silent = ProgressWithStop::new(how_far_along::Unstoppable, NoReport);
    assert_eq!(std::mem::size_of_val(&silent), 0);
    silent.step(1).unwrap();
}

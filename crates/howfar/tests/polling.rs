#![cfg(feature = "alloc")]
use almost_enough::StopToken;
use howfar::poll::{Control, ControlHandle, LocalPoller, PollingStop, SharedPoller};
use howfar::{
    NoProgress, Outcome, Phase, Report, Status, Stop, StopReason, Total, Unstoppable, Work,
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
    let report = job.progress();
    let saved = Rc::new(RefCell::new(None));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let owner = std::thread::current().id();
    let mut poller = LocalPoller::new(job.observer(), ControlHandle::new());
    let seen_first = seen.clone();
    poller.subscribe(move |event| {
        assert_eq!(std::thread::current().id(), owner);
        assert!(!event.snapshot_materialized());
        seen_first
            .borrow_mut()
            .push("arbitrary work without a snapshot");
        Control::Continue
    });
    let saved_first = saved.clone();
    poller.subscribe(move |event| {
        *saved_first.borrow_mut() = Some(event.snapshot_owned());
        report.advance(1); // Changes after the first snapshot must not alter this dispatch.
        Control::Yield
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
        Control::Continue
    });
    let result = poller.poll();
    assert!(result.yield_requested && !result.cancelled);
    assert_eq!(saved.borrow().as_ref().unwrap().completed, 0);
    assert_eq!(seen.borrow().len(), 1);
    assert!(poller.control().take_yield());
    assert!(!poller.control().take_yield());
    job.finish().unwrap();
}

#[test]
fn subscribers_cancel_in_order_and_terminal_poll_bypasses_consumer_cadence() {
    let mut job = Phase::new("job", Total::Exact(4));
    let control = ControlHandle::new();
    let mut poller = LocalPoller::new(job.observer(), control.clone());
    let cancel = poller.subscribe(|_| Control::Cancel);
    poller.subscribe(|event| {
        assert!(event.control().is_cancelled());
        Control::Continue
    });
    assert_eq!(poller.poll().check(), Err(StopReason::Cancelled));
    assert!(poller.unsubscribe(cancel));
    assert!(!poller.unsubscribe(cancel));
    job.finish_with(Outcome::Cancelled).unwrap();
    let saved = Rc::new(RefCell::new(None));
    let copy = saved.clone();
    poller.subscribe(move |event| {
        *copy.borrow_mut() = Some(event.snapshot_owned());
        Control::Continue
    });
    poller.poll(); // Force final delivery regardless of an external timer's deadline.
    assert_eq!(
        saved.borrow().as_ref().unwrap().status,
        Status::Finished(Outcome::Cancelled)
    );
}

#[test]
fn shared_busy_path_observes_cancellation_without_waiting_for_callback() {
    let job = Phase::new("job", Total::Unknown);
    let control = ControlHandle::new();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (exit_tx, exit_rx) = mpsc::channel();
    let exit_rx = Mutex::new(exit_rx);
    let poller = SharedPoller::new(job.observer(), control.clone(), move |_| {
        entered_tx.send(()).unwrap();
        exit_rx.lock().unwrap().recv().unwrap();
        Control::Continue
    });
    std::thread::scope(|scope| {
        scope.spawn(|| poller.try_poll());
        entered_rx.recv().unwrap();
        control.cancel();
        let result = poller.try_poll();
        assert!(result.busy && !result.dispatched && result.cancelled);
        exit_tx.send(()).unwrap();
    });
}

#[test]
fn recursive_dispatch_is_busy_and_unwinding_releases_claim() {
    let job = Phase::new("job", Total::Unknown);
    // A temporary owner slot avoids permanently retaining a self-referential poller.
    let slot = Arc::new(Mutex::new(None::<SharedPoller>));
    let slot_in = slot.clone();
    let panic_once = AtomicBool::new(true);
    let poller = SharedPoller::new(job.observer(), ControlHandle::new(), move |_| {
        let nested = slot_in.lock().unwrap().as_ref().unwrap().clone();
        assert!(nested.try_poll().busy);
        if panic_once.swap(false, Ordering::Relaxed) {
            panic!("subscriber panicked");
        }
        Control::Continue
    });
    *slot.lock().unwrap() = Some(poller.clone());
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| poller.try_poll())).is_err());
    assert!(poller.try_poll().dispatched);
    slot.lock().unwrap().take();
}

#[test]
fn shared_callbacks_are_serialized_and_lazy_snapshot_is_shared() {
    let job = Phase::new("job", Total::Unknown);
    let active = Arc::new(AtomicUsize::new(0));
    let count = Arc::new(AtomicUsize::new(0));
    let mut builder = SharedPoller::builder(job.observer(), ControlHandle::new());
    let a = active.clone();
    let c = count.clone();
    builder.subscribe(move |event| {
        assert_eq!(a.fetch_add(1, Ordering::SeqCst), 0);
        assert!(!event.snapshot_materialized());
        event.snapshot();
        c.fetch_add(1, Ordering::Relaxed);
        a.fetch_sub(1, Ordering::SeqCst);
        Control::Continue
    });
    builder.subscribe(|event| {
        assert!(event.snapshot_materialized());
        Control::Continue
    });
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
fn polling_stop_survives_all_existing_stop_signature_shapes() {
    let job = Phase::new("job", Total::Unknown);
    let callbacks = Arc::new(AtomicUsize::new(0));
    let calls = callbacks.clone();
    let poller = SharedPoller::new(job.observer(), ControlHandle::new(), move |_| {
        calls.fetch_add(1, Ordering::Relaxed);
        Control::Continue
    });
    let hook = PollingStop::new(Unstoppable, poller.clone());
    assert!(hook.may_stop());
    let erased: &dyn Stop = &hook;
    erased.check().unwrap();
    let optional: Option<&dyn Stop> = Some(&hook);
    optional.check().unwrap();
    let owned: Option<StopToken> = Some(StopToken::new(hook.clone()));
    owned.check().unwrap();
    let arc: Option<Arc<dyn Stop>> = Some(Arc::new(hook.clone()));
    arc.check().unwrap();
    fn builder_with_stop(stop: StopToken) {
        stop.check().unwrap();
    }
    builder_with_stop(StopToken::new(Work::new(hook, NoProgress)));
    assert_eq!(callbacks.load(Ordering::Relaxed), 5);
    poller.control().cancel();
    assert_eq!(owned.check(), Err(StopReason::Cancelled));
    assert_eq!(callbacks.load(Ordering::Relaxed), 5);
}

#[test]
fn callback_cancellation_is_returned_at_same_checkpoint_and_inner_checked_once() {
    struct CountingStop(AtomicUsize);
    impl Stop for CountingStop {
        fn check(&self) -> Result<(), StopReason> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }
    let job = Phase::new("job", Total::Unknown);
    let inner = CountingStop(AtomicUsize::new(0));
    let poller = SharedPoller::new(job.observer(), ControlHandle::new(), |_| Control::Cancel);
    let stop = PollingStop::new(&inner, poller);
    assert_eq!(stop.check(), Err(StopReason::Cancelled));
    assert_eq!(inner.0.load(Ordering::Relaxed), 1);
}

#[test]
fn posted_delivery_moves_observation_to_the_owner_thread() {
    let mut job = Phase::new("posted", Total::Exact(1));
    let progress = job.progress();
    let (tx, rx) = mpsc::sync_channel(1);
    let poller = SharedPoller::new(job.observer(), ControlHandle::new(), move |event| {
        let _ = tx.try_send(event.snapshot_owned()); // Consumer chooses bounded/coalesced delivery.
        Control::Continue
    });
    std::thread::scope(|scope| {
        scope.spawn(move || {
            progress.advance(1);
            poller.try_poll();
        });
        assert_eq!(rx.recv().unwrap().completed, 1);
    });
    job.finish().unwrap();
}

#[test]
fn resumable_host_honors_yield_without_cancelling_and_can_then_receive_cancel() {
    let job = Phase::new("chunks", Total::Exact(100));
    let work = Work::new(ControlHandle::new(), job.progress());
    let mut ui = LocalPoller::new(job.observer(), work.stop.clone());
    ui.subscribe(|_| Control::Yield);
    // A host's resumable driver runs a chunk and returns to its scheduler here.
    for _ in 0..10 {
        work.check().unwrap();
        work.advance(1);
    }
    ui.poll();
    assert!(work.stop.take_yield());
    assert!(work.check().is_ok());
    // Only after yielding can the same event loop deliver an incoming cancel message.
    work.stop.cancel();
    assert_eq!(work.check(), Err(StopReason::Cancelled));
    assert_eq!(job.observer().snapshot().completed, 10);
}

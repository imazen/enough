//! Explicit callback dispatch. No timers or threads are started by this module.
//!
//! [`LocalPoller`] permits thread-affine `FnMut` callbacks. [`SharedPoller`]
//! serializes `Send + Sync` callbacks on whichever worker wins a nonblocking
//! claim. Neither holds internal locks while calling application code.
//! A callback panic propagates; a shared dispatch claim is released on unwind.
//!
//! Callbacks may do arbitrary work, including invoking a suspension-enabled
//! Wasm import. An ordinary synchronous callback cannot yield a browser event
//! loop merely by scheduling a timer. [`Control::Yield`] is a request for a
//! resumable driver to honor, not a suspension mechanism or cancellation error.

use crate::{Observer, Snapshot, Stop, StopReason};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    cell::OnceCell,
    sync::atomic::{AtomicBool, Ordering},
};

/// A subscriber's requested action. Cancellation wins over a pending yield.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    /// Continue processing.
    Continue,
    /// Latch cancellation for all workers sharing this control handle.
    Cancel,
    /// Ask a resumable host driver to yield at a safe boundary.
    Yield,
}

#[derive(Default)]
struct Flags {
    cancelled: AtomicBool,
    yield_requested: AtomicBool,
}

/// Shared cancellation and yield requests, independent of reporting and callback cadence.
/// New attempts must use new handles; cancellation cannot be reset.
#[derive(Clone, Default)]
pub struct ControlHandle {
    flags: Arc<Flags>,
}
impl ControlHandle {
    /// Create independent control flags.
    pub fn new() -> Self {
        Self::default()
    }
    /// Request cancellation immediately, without invoking a subscriber.
    pub fn cancel(&self) {
        self.flags.cancelled.store(true, Ordering::Release);
    }
    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.flags.cancelled.load(Ordering::Acquire)
    }
    /// Request a cooperative yield without making `Stop::check` fail.
    pub fn request_yield(&self) {
        self.flags.yield_requested.store(true, Ordering::Release);
    }
    /// Consume the coalesced yield request. A single host driver should own this operation.
    pub fn take_yield(&self) -> bool {
        self.flags.yield_requested.swap(false, Ordering::AcqRel)
    }
    /// Inspect a yield request without consuming it.
    pub fn yield_requested(&self) -> bool {
        self.flags.yield_requested.load(Ordering::Acquire)
    }
    fn apply(&self, action: Control) {
        match action {
            Control::Continue => {}
            Control::Cancel => self.cancel(),
            Control::Yield => self.request_yield(),
        }
    }
    fn outcome(&self, dispatched: bool, busy: bool) -> PollOutcome {
        PollOutcome {
            cancelled: self.is_cancelled(),
            yield_requested: self.yield_requested(),
            dispatched,
            busy,
        }
    }
}
impl Stop for ControlHandle {
    #[inline]
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        if self.is_cancelled() {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// One dispatch's borrowed context. A snapshot is built only on first request,
/// at the time of that request; all subscribers in this dispatch share it.
pub struct PollEvent<'a> {
    observer: &'a Observer,
    control: &'a ControlHandle,
    snapshot: OnceCell<Arc<Snapshot>>,
}
impl<'a> PollEvent<'a> {
    fn new(observer: &'a Observer, control: &'a ControlHandle) -> Self {
        Self {
            observer,
            control,
            snapshot: OnceCell::new(),
        }
    }
    /// Materialize this dispatch's observation lazily.
    pub fn snapshot(&self) -> &Snapshot {
        self.snapshot_arc().as_ref()
    }
    /// Retain **this** observation for deferred rendering or posted delivery.
    pub fn snapshot_owned(&self) -> Arc<Snapshot> {
        Arc::clone(self.snapshot_arc())
    }
    /// Retain an observer to obtain a **later** observation instead.
    pub fn observer(&self) -> &Observer {
        self.observer
    }
    /// Request cancellation/yield directly, even from a callback returning Continue.
    pub fn control(&self) -> &ControlHandle {
        self.control
    }
    /// Whether any subscriber has requested the snapshot yet.
    pub fn snapshot_materialized(&self) -> bool {
        self.snapshot.get().is_some()
    }
    fn snapshot_arc(&self) -> &Arc<Snapshot> {
        self.snapshot
            .get_or_init(|| Arc::new(self.observer.snapshot()))
    }
}

/// Dispatch result; busy workers still observe cancellation and yield requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PollOutcome {
    /// Cancellation has been latched.
    pub cancelled: bool,
    /// A host yield is pending; it is not automatically consumed.
    pub yield_requested: bool,
    /// This call obtained a dispatch turn (possibly with no subscribers).
    pub dispatched: bool,
    /// Another call is dispatching, including a recursive call to this poller.
    pub busy: bool,
}
impl PollOutcome {
    /// Convert only cancellation into the established Stop error model.
    pub fn check(self) -> Result<(), StopReason> {
        if self.cancelled {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Opaque identifier used to remove a subscriber from its poller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubscriptionId(usize);
type LocalCallback<'a> = Box<dyn FnMut(&PollEvent<'_>) -> Control + 'a>;
type SharedCallback = Arc<dyn Fn(&PollEvent<'_>) -> Control + Send + Sync>;

/// Owner-thread polling. Callbacks may capture `Rc`, DOM handles, and borrowed state.
/// Poll after joining once more to deliver the terminal snapshot regardless of cadence.
pub struct LocalPoller<'a> {
    observer: Observer,
    control: ControlHandle,
    subscribers: Vec<(SubscriptionId, LocalCallback<'a>)>,
    next_id: usize,
}
impl<'a> LocalPoller<'a> {
    /// Observe a subtree using an independently chosen job control handle.
    pub fn new(observer: Observer, control: ControlHandle) -> Self {
        Self {
            observer,
            control,
            subscribers: Vec::new(),
            next_id: 0,
        }
    }
    /// Register arbitrary owner-thread work. No snapshot is built unless requested.
    pub fn subscribe(
        &mut self,
        callback: impl FnMut(&PollEvent<'_>) -> Control + 'a,
    ) -> SubscriptionId {
        let id = SubscriptionId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("subscription identifiers exhausted");
        self.subscribers.push((id, Box::new(callback)));
        id
    }
    /// Remove a subscription. Returns whether it existed in this poller.
    pub fn unsubscribe(&mut self, id: SubscriptionId) -> bool {
        let before = self.subscribers.len();
        self.subscribers.retain(|(key, _)| *key != id);
        before != self.subscribers.len()
    }
    /// Invoke subscribers here, once, in registration order. Cancellation from
    /// an earlier callback is visible to later callbacks in the same dispatch.
    pub fn poll(&mut self) -> PollOutcome {
        let event = PollEvent::new(&self.observer, &self.control);
        for (_, callback) in &mut self.subscribers {
            self.control.apply(callback(&event));
        }
        self.control.outcome(true, false)
    }
    /// Inspect or clone shared control independently of delivery.
    pub fn control(&self) -> &ControlHandle {
        &self.control
    }
}

struct SharedInner {
    observer: Observer,
    control: ControlHandle,
    subscribers: Vec<SharedCallback>,
    dispatching: AtomicBool,
}

/// Clonable, nonblocking callback dispatch on a polling worker's thread.
/// Callbacks are serialized per poller, but their executing thread may change.
/// GUI callbacks belong in LocalPoller or an application-owned posted-message queue.
#[derive(Clone)]
pub struct SharedPoller {
    inner: Arc<SharedInner>,
}
impl SharedPoller {
    /// Create a dispatcher with one callback. It may notify multiple downstream
    /// consumers, or use [`Self::builder`] to configure a fixed subscriber list.
    pub fn new(
        observer: Observer,
        control: ControlHandle,
        callback: impl Fn(&PollEvent<'_>) -> Control + Send + Sync + 'static,
    ) -> Self {
        let mut builder = Self::builder(observer, control);
        builder.subscribe(callback);
        builder.build()
    }
    /// Configure multiple subscribers before sharing the dispatcher with workers.
    /// The immutable list needs no registry lock on either browser or native threads.
    pub fn builder(observer: Observer, control: ControlHandle) -> SharedPollerBuilder {
        SharedPollerBuilder {
            observer,
            control,
            subscribers: Vec::new(),
        }
    }
    /// Try one dispatch. Busy/recursive calls do not wait for arbitrary callback
    /// work. The dispatch claim is restored even if a callback panics.
    pub fn try_poll(&self) -> PollOutcome {
        if self
            .inner
            .dispatching
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return self.inner.control.outcome(false, true);
        }
        struct Claim<'a>(&'a AtomicBool);
        impl Drop for Claim<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _claim = Claim(&self.inner.dispatching);
        let event = PollEvent::new(&self.inner.observer, &self.inner.control);
        for callback in &self.inner.subscribers {
            self.inner.control.apply(callback(&event));
        }
        self.inner.control.outcome(true, false)
    }
    /// Shared cancellation/yield flags remain usable while dispatch is busy.
    pub fn control(&self) -> &ControlHandle {
        &self.inner.control
    }
}

/// Configure a shared dispatcher before handing it to workers. Subscription
/// lifetime is the dispatcher's lifetime; rebuild the dispatcher to change it.
/// This avoids locks and retired callback retention on the browser main thread.
pub struct SharedPollerBuilder {
    observer: Observer,
    control: ControlHandle,
    subscribers: Vec<SharedCallback>,
}
impl SharedPollerBuilder {
    /// Append a callback in dispatch order.
    pub fn subscribe(
        &mut self,
        callback: impl Fn(&PollEvent<'_>) -> Control + Send + Sync + 'static,
    ) -> &mut Self {
        self.subscribers.push(Arc::new(callback));
        self
    }
    /// Freeze the subscriber list and make a clonable dispatcher.
    pub fn build(self) -> SharedPoller {
        SharedPoller {
            inner: Arc::new(SharedInner {
                observer: self.observer,
                control: self.control,
                subscribers: self.subscribers,
                dispatching: AtomicBool::new(false),
            }),
        }
    }
}

/// Drive shared subscribers through existing `Stop::check` call sites.
///
/// Every check first checks shared cancellation, then the wrapped stop exactly
/// once, then (if still running) dispatches and rechecks shared cancellation.
/// No check means no dispatch. Use the poller directly for forced final delivery.
/// `may_stop` is conservatively true so StopToken cannot erase callback effects.
/// Yield requests remain on the control handle for a capable host driver.
#[derive(Clone)]
pub struct PollingStop<S> {
    stop: S,
    poller: SharedPoller,
}
impl<S> PollingStop<S> {
    /// Explicitly opt existing cancellation checkpoints into callback work.
    pub fn new(stop: S, poller: SharedPoller) -> Self {
        Self { stop, poller }
    }
    /// Access the dispatcher, including its control handle.
    pub fn poller(&self) -> &SharedPoller {
        &self.poller
    }
}
impl<S: Stop> Stop for PollingStop<S> {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.poller.control().check()?;
        self.stop.check()?;
        self.poller.try_poll().check()
    }
}

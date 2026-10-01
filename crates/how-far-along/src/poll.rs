//! Callbacks over snapshots, dispatched when you poll.
//!
//! This module starts no threads and reads no clocks: your code decides when
//! to poll. [`LocalPoller`] runs `FnMut` callbacks on the thread that polls,
//! so they may capture `Rc`, UI handles, and borrowed state. [`SharedPoller`]
//! runs `Send + Sync` callbacks on whichever worker polls first; a worker that
//! finds a dispatch already running returns immediately instead of waiting.
//!
//! A snapshot is built only if a callback asks for one, at most once per
//! dispatch. Callbacks run outside every internal lock, and a panicking
//! callback propagates after releasing the shared dispatch claim.
//!
//! Cancellation is not part of polling. To let a callback stop the work, give
//! it a clone of the stop policy, such as an `almost_enough::Stopper`.

use crate::{Observer, Snapshot};
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    cell::OnceCell,
    fmt,
    sync::atomic::{AtomicBool, Ordering},
};

/// One dispatch, as seen by its callbacks. The snapshot is built on first
/// request and shared by every later callback in the same dispatch.
pub struct PollEvent<'a> {
    observer: &'a Observer,
    snapshot: OnceCell<Arc<Snapshot>>,
}

impl<'a> PollEvent<'a> {
    fn new(observer: &'a Observer) -> Self {
        Self {
            observer,
            snapshot: OnceCell::new(),
        }
    }
    /// This dispatch's snapshot, built on first use.
    pub fn snapshot(&self) -> &Snapshot {
        self.snapshot
            .get_or_init(|| Arc::new(self.observer.snapshot()))
            .as_ref()
    }
    /// This dispatch's snapshot, unless building it would wait for another
    /// thread. UI callbacks use this and try again on their next turn.
    pub fn try_snapshot(&self) -> Option<&Snapshot> {
        if self.snapshot.get().is_none() {
            let snapshot = Arc::new(self.observer.try_snapshot()?);
            let _ = self.snapshot.set(snapshot);
        }
        self.snapshot.get().map(Arc::as_ref)
    }
    /// Keep **this** dispatch's snapshot, for example to render it later or
    /// post it to another thread.
    pub fn snapshot_owned(&self) -> Arc<Snapshot> {
        self.snapshot();
        Arc::clone(self.snapshot.get().expect("snapshot was just built"))
    }
    /// The observer, for taking a **later** snapshot instead.
    pub fn observer(&self) -> &Observer {
        self.observer
    }
    /// Whether a callback has already built this dispatch's snapshot.
    pub fn snapshot_materialized(&self) -> bool {
        self.snapshot.get().is_some()
    }
}

impl fmt::Debug for PollEvent<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PollEvent")
            .field("snapshot_materialized", &self.snapshot_materialized())
            .finish_non_exhaustive()
    }
}

/// Identifies a [`LocalPoller`] subscription, for removal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SubscriptionId(usize);

type LocalCallback<'a> = Box<dyn FnMut(&PollEvent<'_>) + 'a>;
type SharedCallback = Box<dyn Fn(&PollEvent<'_>) + Send + Sync>;

/// Callbacks that run on the polling thread, in subscription order.
///
/// Poll from your UI loop or timer at whatever cadence you choose, and once
/// more after the work joins to deliver the final snapshot.
pub struct LocalPoller<'a> {
    observer: Observer,
    subscribers: Vec<(SubscriptionId, LocalCallback<'a>)>,
    next_id: usize,
}

impl<'a> LocalPoller<'a> {
    /// Dispatch over snapshots of `observer`'s subtree.
    pub fn new(observer: Observer) -> Self {
        Self {
            observer,
            subscribers: Vec::new(),
            next_id: 0,
        }
    }
    /// Add a callback. It may do arbitrary work; no snapshot is built unless
    /// it asks for one.
    pub fn subscribe(&mut self, callback: impl FnMut(&PollEvent<'_>) + 'a) -> SubscriptionId {
        let id = SubscriptionId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("subscription identifiers exhausted");
        self.subscribers.push((id, Box::new(callback)));
        id
    }
    /// Remove a subscription. Returns whether it was present.
    pub fn unsubscribe(&mut self, id: SubscriptionId) -> bool {
        let before = self.subscribers.len();
        self.subscribers.retain(|(key, _)| *key != id);
        before != self.subscribers.len()
    }
    /// Run every callback once, in subscription order.
    pub fn poll(&mut self) {
        let event = PollEvent::new(&self.observer);
        for (_, callback) in &mut self.subscribers {
            callback(&event);
        }
    }
}

impl fmt::Debug for LocalPoller<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalPoller")
            .field("subscribers", &self.subscribers.len())
            .finish_non_exhaustive()
    }
}

struct SharedInner {
    observer: Observer,
    subscribers: Vec<SharedCallback>,
    dispatching: AtomicBool,
}

/// Cloneable callback dispatch for worker threads.
///
/// Any worker may call [`try_poll`](Self::try_poll); callbacks never run
/// concurrently with each other, but the thread that runs them can change from
/// one dispatch to the next. Keep thread-affine work, such as GUI updates, in
/// a [`LocalPoller`] or post it to the owning thread from the callback.
#[derive(Clone)]
pub struct SharedPoller {
    inner: Arc<SharedInner>,
}

impl SharedPoller {
    /// Dispatch one callback over snapshots of `observer`'s subtree. For
    /// several callbacks, use [`builder`](Self::builder).
    pub fn new(
        observer: Observer,
        callback: impl Fn(&PollEvent<'_>) + Send + Sync + 'static,
    ) -> Self {
        let mut builder = Self::builder(observer);
        builder.subscribe(callback);
        builder.build()
    }
    /// Collect callbacks before the poller is shared. The list is fixed once
    /// built, so dispatch needs no lock on any platform.
    pub fn builder(observer: Observer) -> SharedPollerBuilder {
        SharedPollerBuilder {
            observer,
            subscribers: Vec::new(),
        }
    }
    /// Run every callback once if no other dispatch is running.
    ///
    /// Returns `false` without waiting if another thread, or a callback of this
    /// poller, is already dispatching.
    pub fn try_poll(&self) -> bool {
        if self
            .inner
            .dispatching
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return false;
        }
        struct Claim<'a>(&'a AtomicBool);
        impl Drop for Claim<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _claim = Claim(&self.inner.dispatching);
        let event = PollEvent::new(&self.inner.observer);
        for callback in &self.inner.subscribers {
            callback(&event);
        }
        true
    }
}

impl fmt::Debug for SharedPoller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedPoller")
            .field("subscribers", &self.inner.subscribers.len())
            .finish_non_exhaustive()
    }
}

/// Collects callbacks for a [`SharedPoller`].
pub struct SharedPollerBuilder {
    observer: Observer,
    subscribers: Vec<SharedCallback>,
}

impl SharedPollerBuilder {
    /// Add a callback, dispatched after those added before it.
    pub fn subscribe(
        &mut self,
        callback: impl Fn(&PollEvent<'_>) + Send + Sync + 'static,
    ) -> &mut Self {
        self.subscribers.push(Box::new(callback));
        self
    }
    /// Fix the callback list and build a cloneable poller.
    pub fn build(self) -> SharedPoller {
        SharedPoller {
            inner: Arc::new(SharedInner {
                observer: self.observer,
                subscribers: self.subscribers,
                dispatching: AtomicBool::new(false),
            }),
        }
    }
}

impl fmt::Debug for SharedPollerBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedPollerBuilder")
            .field("subscribers", &self.subscribers.len())
            .finish_non_exhaustive()
    }
}

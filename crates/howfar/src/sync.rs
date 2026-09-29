//! Internal synchronization. No spin locks, blocking reads, or external dependencies.
//!
//! Published versions stay alive until the containing job is dropped. Only cold
//! phase-plan/total/outcome changes publish versions; counters never do. This
//! intentionally exchanges per-job metadata retention for wait-free reads.

use alloc::boxed::Box;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

struct Version<T> {
    value: T,
    previous: *mut Version<T>,
}
pub(crate) struct Published<T> {
    head: AtomicPtr<Version<T>>,
}

// SAFETY: values become immutable before publication and are retained until
// exclusive Drop. All access to the head pointer is atomic. Moving/sharing the
// container requires the retained values themselves to be Send + Sync.
unsafe impl<T: Send + Sync> Send for Published<T> {}
unsafe impl<T: Send + Sync> Sync for Published<T> {}

impl<T> Published<T> {
    pub(crate) fn new(value: T) -> Self {
        Self {
            head: AtomicPtr::new(Box::into_raw(Box::new(Version {
                value,
                previous: core::ptr::null_mut(),
            }))),
        }
    }
    pub(crate) fn get(&self) -> &T {
        let head = self.head.load(Ordering::Acquire);
        // SAFETY: head is initialized, never null, and every previously published
        // version remains allocated for the duration of this borrow of self.
        unsafe { &(*head).value }
    }
    /// Sole writer: Phase's &mut methods. Old values remain immutable and retained.
    pub(crate) fn publish(&self, value: T) {
        let previous = self.head.load(Ordering::Relaxed);
        let next = Box::into_raw(Box::new(Version { value, previous }));
        self.head.store(next, Ordering::Release);
    }
}
impl<T> Drop for Published<T> {
    fn drop(&mut self) {
        let mut current = *self.head.get_mut();
        while !current.is_null() {
            // SAFETY: exclusive Drop means no readers/writers remain. Each
            // version was allocated once with Box::into_raw and is freed once.
            let version = unsafe { Box::from_raw(current) };
            current = version.previous;
            drop(version);
        }
    }
}

#[cfg(feature = "profile")]
pub(crate) struct Mutex<T>(std::sync::Mutex<T>);
#[cfg(feature = "profile")]
impl<T> Mutex<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self(std::sync::Mutex::new(value))
    }
    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, T> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(crate) fn try_lock(&self) -> Option<std::sync::MutexGuard<'_, T>> {
        match self.0.try_lock() {
            Ok(guard) => Some(guard),
            Err(std::sync::TryLockError::Poisoned(error)) => Some(error.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }
}

// On targets with only 32-bit atomics, expose native-counter saturation instead
// of adding a hidden lock. Snapshot::counter_max() documents the target limit.
#[cfg(target_has_atomic = "64")]
type AtomicCount = core::sync::atomic::AtomicU64;
#[cfg(not(target_has_atomic = "64"))]
type AtomicCount = core::sync::atomic::AtomicUsize;
pub(crate) struct Counter {
    value: AtomicCount,
    overflow: AtomicBool,
}
impl Counter {
    pub(crate) const fn new() -> Self {
        Self {
            value: AtomicCount::new(0),
            overflow: AtomicBool::new(false),
        }
    }
    #[allow(clippy::unnecessary_cast)] // AtomicCount is target-dependent.
    pub(crate) fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed) as u64
    }
    pub(crate) fn overflowed(&self) -> bool {
        self.overflow.load(Ordering::Relaxed)
    }
    #[allow(clippy::unnecessary_cast)] // AtomicCount is target-dependent.
    #[allow(deprecated)] // Atomic::try_update is newer than the Rust 1.88 MSRV.
    pub(crate) fn add(&self, n: u64) {
        #[cfg(target_has_atomic = "64")]
        let delta = n;
        #[cfg(not(target_has_atomic = "64"))]
        let delta = n.min(usize::MAX as u64) as usize;
        let old = self
            .value
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_add(delta))
            })
            .expect("update always succeeds");
        let overflow = old.checked_add(delta).is_none() || delta as u64 != n;
        if overflow {
            self.overflow.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use alloc::sync::Arc;
    use core::sync::atomic::AtomicUsize;
    struct Value(Arc<AtomicUsize>, usize);
    impl Drop for Value {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    #[test]
    fn immutable_versions_stay_valid_and_are_reclaimed_exactly_once() {
        let drops = Arc::new(AtomicUsize::new(0));
        let published = Published::new(Value(drops.clone(), 0));
        let first = published.get();
        for i in 1..50 {
            published.publish(Value(drops.clone(), i));
        }
        assert_eq!(first.1, 0);
        assert_eq!(published.get().1, 49);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(published);
        assert_eq!(drops.load(Ordering::Relaxed), 50);
    }
    #[test]
    fn readers_overlap_publication_without_reclaiming_borrowed_versions() {
        let published = Published::new(0_usize);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..50 {
                    let version = published.get();
                    std::thread::yield_now();
                    assert!(*version < 50);
                }
            });
            for i in 1..50 {
                published.publish(i);
            }
        });
        assert_eq!(*published.get(), 49);
    }
}

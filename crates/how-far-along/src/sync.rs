//! Safe synchronization for consumer-owned metadata. Reports remain atomic.
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

/// A unique owner, administered under a short lock. No user code runs inside
/// it, so concurrent administrators queue instead of failing, and Drop takes
/// the owner under the same lock and destroys it after releasing it.
pub(crate) struct OwnerCell<T> {
    #[cfg(feature = "std")]
    value: std::sync::Mutex<Option<T>>,
    #[cfg(not(feature = "std"))]
    value: critical_section::Mutex<core::cell::RefCell<Option<T>>>,
}
impl<T> OwnerCell<T> {
    pub(crate) fn new(value: T) -> Self {
        Self {
            #[cfg(feature = "std")]
            value: std::sync::Mutex::new(Some(value)),
            #[cfg(not(feature = "std"))]
            value: critical_section::Mutex::new(core::cell::RefCell::new(Some(value))),
        }
    }
    pub(crate) fn take(&self) -> Option<T> {
        #[cfg(feature = "std")]
        {
            self.value
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
        }
        #[cfg(not(feature = "std"))]
        {
            critical_section::with(|cs| self.value.borrow(cs).borrow_mut().take())
        }
    }
    /// Administer the owner, if it is still present, under the lock.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        #[cfg(feature = "std")]
        {
            self.value
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_mut()
                .map(f)
        }
        #[cfg(not(feature = "std"))]
        {
            critical_section::with(|cs| self.value.borrow(cs).borrow_mut().as_mut().map(f))
        }
    }
}

/// Copy an Arc under a short platform lock; cloning metadata and walking trees
/// happen after releasing it. Replaced versions die when their last reader drops.
pub(crate) struct MetadataCell<T> {
    #[cfg(feature = "std")]
    value: std::sync::Mutex<Arc<T>>,
    #[cfg(not(feature = "std"))]
    value: critical_section::Mutex<core::cell::RefCell<Arc<T>>>,
}
impl<T> MetadataCell<T> {
    #[cfg(all(test, feature = "std"))]
    pub(crate) fn with_lock_for_test(&self, test: impl FnOnce()) {
        let _guard = self.value.lock().unwrap();
        test();
    }
    pub(crate) fn new(value: T) -> Self {
        let value = Arc::new(value);
        Self {
            #[cfg(feature = "std")]
            value: std::sync::Mutex::new(value),
            #[cfg(not(feature = "std"))]
            value: critical_section::Mutex::new(core::cell::RefCell::new(value)),
        }
    }
    pub(crate) fn get(&self) -> Arc<T> {
        #[cfg(feature = "std")]
        {
            Arc::clone(
                &self
                    .value
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            )
        }
        #[cfg(not(feature = "std"))]
        {
            critical_section::with(|cs| Arc::clone(&self.value.borrow(cs).borrow()))
        }
    }
    pub(crate) fn try_get(&self) -> Option<Arc<T>> {
        #[cfg(feature = "std")]
        {
            match self.value.try_lock() {
                Ok(guard) => Some(Arc::clone(&guard)),
                Err(std::sync::TryLockError::Poisoned(error)) => {
                    Some(Arc::clone(&error.into_inner()))
                }
                Err(std::sync::TryLockError::WouldBlock) => None,
            }
        }
        #[cfg(not(feature = "std"))]
        {
            critical_section::with(|cs| {
                self.value
                    .borrow(cs)
                    .try_borrow()
                    .ok()
                    .map(|value| Arc::clone(&value))
            })
        }
    }
    pub(crate) fn publish(&self, value: T) {
        let next = Arc::new(value);
        // Neither allocation nor destruction occurs under the metadata lock.
        #[cfg(feature = "std")]
        let previous = core::mem::replace(
            &mut *self
                .value
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            next,
        );
        #[cfg(not(feature = "std"))]
        let previous = critical_section::with(|cs| self.value.borrow(cs).replace(next));
        drop(previous);
    }
}

// On targets with only 32-bit atomics, expose native-counter saturation instead
// of adding a hidden lock. Snapshot::counter_max() documents the target limit.
#[cfg(target_has_atomic = "64")]
type AtomicCount = core::sync::atomic::AtomicU64;
#[cfg(not(target_has_atomic = "64"))]
type AtomicCount = core::sync::atomic::AtomicUsize;

/// The largest count; reports past it saturate and set the overflow flag.
///
/// With 64-bit atomics this is `2^63 - 1`, leaving headroom so that a report
/// below [`FAST_ADD`] is one `fetch_add`, which never retries when workers
/// share a phase. A report that takes the count past the limit stores the
/// limit back, so the excess is at most the reports in flight, one per
/// thread, each below 2^32: wrapping would take 2^31 threads reporting at
/// once. Larger reports saturate in a compare-and-swap loop.
#[cfg(target_has_atomic = "64")]
pub(crate) const COUNT_MAX: u64 = (1 << 63) - 1;
#[cfg(not(target_has_atomic = "64"))]
pub(crate) const COUNT_MAX: u64 = usize::MAX as u64;
#[cfg(target_has_atomic = "64")]
const FAST_ADD: u64 = 1 << 32;

pub(crate) struct Counter {
    /// May exceed `COUNT_MAX` by the reports in flight once it saturates;
    /// readers see at most `COUNT_MAX`.
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
        (self.value.load(Ordering::Relaxed) as u64).min(COUNT_MAX)
    }
    pub(crate) fn overflowed(&self) -> bool {
        self.overflow.load(Ordering::Relaxed)
    }
    /// Take `source`'s current count and overflow flag.
    pub(crate) fn copy_from(&self, source: &Counter) {
        self.value
            .store(source.value.load(Ordering::Relaxed), Ordering::Relaxed);
        self.overflow.store(source.overflowed(), Ordering::Relaxed);
    }
    #[allow(clippy::unnecessary_cast)] // AtomicCount is target-dependent.
    #[allow(deprecated)] // Atomic::try_update is newer than the Rust 1.88 MSRV.
    pub(crate) fn add(&self, n: u64) {
        #[cfg(target_has_atomic = "64")]
        if n < FAST_ADD {
            // Cannot wrap; see COUNT_MAX. No load first: under contention it
            // would fetch the cache line twice.
            let old = self.value.fetch_add(n, Ordering::Relaxed);
            if old.saturating_add(n) > COUNT_MAX {
                self.value.store(COUNT_MAX, Ordering::Relaxed);
                self.overflow.store(true, Ordering::Relaxed);
            }
            return;
        }
        #[cfg(target_has_atomic = "64")]
        let delta = n;
        #[cfg(not(target_has_atomic = "64"))]
        let delta = n.min(usize::MAX as u64) as usize;
        let max = COUNT_MAX as _;
        let old = self
            .value
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_add(delta).min(max))
            })
            .expect("update always succeeds");
        if (old as u64).saturating_add(n) > COUNT_MAX {
            self.overflow.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::sync::atomic::AtomicUsize;
    struct Value(Arc<AtomicUsize>, usize);
    impl Drop for Value {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    #[test]
    fn readers_retain_versions_and_replaced_metadata_is_reclaimed() {
        let drops = Arc::new(AtomicUsize::new(0));
        let cell = MetadataCell::new(Value(drops.clone(), 0));
        let first = cell.get();
        for i in 1..50 {
            cell.publish(Value(drops.clone(), i));
        }
        assert_eq!(first.1, 0);
        assert_eq!(cell.get().1, 49);
        assert_eq!(drops.load(Ordering::Relaxed), 48);
        drop(cell);
        assert_eq!(drops.load(Ordering::Relaxed), 49);
        drop(first);
        assert_eq!(drops.load(Ordering::Relaxed), 50);
    }
    #[test]
    fn readers_overlap_metadata_replacement() {
        let cell = MetadataCell::new(0_usize);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..50 {
                    let version = cell.get();
                    std::thread::yield_now();
                    assert!(*version < 50);
                }
            });
            for i in 1..50 {
                cell.publish(i);
            }
        });
        assert_eq!(*cell.get(), 49);
    }

    #[test]
    fn counts_exactly_and_saturates_at_the_limit() {
        let counter = Counter::new();
        counter.add(5);
        counter.add(0);
        assert_eq!((counter.get(), counter.overflowed()), (5, false));
        // A large report takes the saturating path.
        counter.add(COUNT_MAX - 7);
        assert_eq!(
            (counter.get(), counter.overflowed()),
            (COUNT_MAX - 2, false)
        );
        counter.add(2);
        assert_eq!((counter.get(), counter.overflowed()), (COUNT_MAX, false));
        // A small report past the limit overshoots internally; readers and
        // later reports see the limit.
        counter.add(1);
        assert_eq!((counter.get(), counter.overflowed()), (COUNT_MAX, true));
        counter.add(u64::MAX);
        counter.add(3);
        assert_eq!((counter.get(), counter.overflowed()), (COUNT_MAX, true));
        let copy = Counter::new();
        copy.copy_from(&counter);
        assert_eq!((copy.get(), copy.overflowed()), (COUNT_MAX, true));
    }

    #[test]
    fn workers_count_exactly_and_saturate_together() {
        let counter = Counter::new();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| (0..10_000).for_each(|_| counter.add(3)));
            }
        });
        assert_eq!((counter.get(), counter.overflowed()), (120_000, false));
        // Race small reports across the limit: none may wrap the count.
        let counter = Counter::new();
        counter.add(COUNT_MAX - 1_000);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..1_000 {
                        counter.add(7);
                        assert!(counter.get() >= COUNT_MAX - 1_000);
                    }
                });
            }
        });
        assert_eq!((counter.get(), counter.overflowed()), (COUNT_MAX, true));
    }
}

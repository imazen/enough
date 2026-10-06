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
}

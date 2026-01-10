//! Zero-allocation cancellation primitives.
//!
//! This module provides stack-based cancellation using borrowed references.
//! Works in `no_std` environments without an allocator.
//!
//! # Overview
//!
//! - [`StopSource`] - A cancellation source that owns an `AtomicBool` on the stack
//! - [`StopRef`] - A borrowed reference to check cancellation
//!
//! # Example
//!
//! ```rust
//! use enough::{StopSource, Stop};
//!
//! let source = StopSource::new();
//! let stop = source.as_ref();
//!
//! assert!(!stop.should_stop());
//!
//! source.cancel();
//! assert!(stop.should_stop());
//! ```
//!
//! # When to Use
//!
//! Use `StopSource`/`StopRef` when:
//! - You need zero-allocation cancellation
//! - The source outlives all references (stack-based usage)
//! - You're in a `no_std` environment
//!
//! Use [`Stopper`](crate::Stopper) when:
//! - You need to share ownership (clone instead of borrow)
//! - You want to pass stops across thread boundaries without lifetimes

use core::sync::atomic::{AtomicBool, Ordering};

use crate::{Stop, StopReason};

/// A stack-based cancellation source.
///
/// This is a zero-allocation cancellation primitive. The source owns the
/// atomic and can issue borrowed references via [`as_ref()`](Self::as_ref).
///
/// # Example
///
/// ```rust
/// use enough::{StopSource, Stop};
///
/// let source = StopSource::new();
/// let stop = source.as_ref();
///
/// // Check in your operation
/// assert!(!stop.should_stop());
///
/// // Cancel when needed
/// source.cancel();
/// assert!(stop.should_stop());
/// ```
///
/// # Const Construction
///
/// `StopSource` can be created in const context:
///
/// ```rust
/// use enough::StopSource;
///
/// static GLOBAL_STOP: StopSource = StopSource::new();
/// ```
#[derive(Debug)]
pub struct StopSource {
    cancelled: AtomicBool,
}

impl StopSource {
    /// Create a new cancellation source.
    #[inline]
    pub const fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
        }
    }

    /// Create a source that is already cancelled.
    ///
    /// Useful for testing or when you want to signal immediate stop.
    #[inline]
    pub const fn cancelled() -> Self {
        Self {
            cancelled: AtomicBool::new(true),
        }
    }

    /// Signal all references to stop.
    ///
    /// This is idempotent - calling it multiple times has no additional effect.
    #[inline]
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Check if this source has been cancelled.
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Get a borrowed reference to pass to operations.
    ///
    /// The reference borrows from this source, so it cannot outlive it.
    /// For owned stops, use [`Stopper`](crate::Stopper).
    #[inline]
    pub fn as_ref(&self) -> StopRef<'_> {
        StopRef {
            cancelled: &self.cancelled,
        }
    }

    /// Alias for [`as_ref()`](Self::as_ref) for migration from AtomicStop.
    #[inline]
    #[deprecated(since = "0.1.0", note = "use as_ref() instead")]
    pub fn token(&self) -> StopRef<'_> {
        self.as_ref()
    }
}

impl Default for StopSource {
    fn default() -> Self {
        Self::new()
    }
}

impl Stop for StopSource {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }

    #[inline]
    fn should_stop(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// A borrowed reference to a [`StopSource`].
///
/// This is a lightweight reference that can only check for cancellation -
/// it cannot trigger it. Use the source to cancel.
///
/// # Example
///
/// ```rust
/// use enough::{StopSource, Stop};
///
/// fn process(data: &[u8], stop: impl Stop) {
///     for (i, chunk) in data.chunks(100).enumerate() {
///         if i % 10 == 0 && stop.should_stop() {
///             return;
///         }
///         // process chunk...
///     }
/// }
///
/// let source = StopSource::new();
/// process(&[0u8; 1000], source.as_ref());
/// ```
///
/// # Copy Semantics
///
/// `StopRef` is `Copy`, so you can freely copy it without cloning:
///
/// ```rust
/// use enough::{StopSource, Stop};
///
/// let source = StopSource::new();
/// let r1 = source.as_ref();
/// let r2 = r1;  // Copy
/// let r3 = r1;  // Still valid
///
/// source.cancel();
/// assert!(r1.should_stop());
/// assert!(r2.should_stop());
/// assert!(r3.should_stop());
/// ```
#[derive(Debug, Clone, Copy)]
pub struct StopRef<'a> {
    cancelled: &'a AtomicBool,
}

impl Stop for StopRef<'_> {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }

    #[inline]
    fn should_stop(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_source_basic() {
        let source = StopSource::new();
        assert!(!source.is_cancelled());
        assert!(!source.should_stop());
        assert!(source.check().is_ok());

        source.cancel();

        assert!(source.is_cancelled());
        assert!(source.should_stop());
        assert_eq!(source.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn stop_source_cancelled_constructor() {
        let source = StopSource::cancelled();
        assert!(source.is_cancelled());
        assert!(source.should_stop());
    }

    #[test]
    fn stop_ref_basic() {
        let source = StopSource::new();
        let stop = source.as_ref();

        assert!(!stop.should_stop());
        assert!(stop.check().is_ok());

        source.cancel();

        assert!(stop.should_stop());
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn stop_ref_is_copy() {
        let source = StopSource::new();
        let r1 = source.as_ref();
        let r2 = r1; // Copy
        let _ = r1; // Still valid
        let _ = r2;
    }

    #[test]
    fn stop_source_is_default() {
        let source: StopSource = Default::default();
        assert!(!source.is_cancelled());
    }

    #[test]
    fn stop_source_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StopSource>();
        assert_send_sync::<StopRef<'_>>();
    }

    #[test]
    fn cancel_is_idempotent() {
        let source = StopSource::new();
        source.cancel();
        source.cancel();
        source.cancel();
        assert!(source.is_cancelled());
    }

    #[test]
    fn const_construction() {
        static SOURCE: StopSource = StopSource::new();
        assert!(!SOURCE.is_cancelled());
    }
}

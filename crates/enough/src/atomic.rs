//! Atomic cancellation primitives.
//!
//! This module provides zero-allocation cancellation using `AtomicBool`.
//! Works in `no_std` environments without an allocator.
//!
//! # Overview
//!
//! - [`AtomicStop`] - A cancellation source that owns an `AtomicBool`
//! - [`AtomicToken`] - A borrowed reference to check cancellation
//!
//! # Example
//!
//! ```rust
//! use enough::{AtomicStop, Stop};
//!
//! let source = AtomicStop::new();
//! let token = source.token();
//!
//! assert!(!token.should_stop());
//!
//! source.cancel();
//! assert!(token.should_stop());
//! ```
//!
//! # When to Use
//!
//! Use `AtomicStop`/`AtomicToken` when:
//! - You need zero-allocation cancellation
//! - The source outlives all tokens (stack-based usage)
//! - You're in a `no_std` environment
//!
//! Use [`ArcStop`](crate::ArcStop)/[`ArcToken`](crate::ArcToken) when:
//! - You need owned tokens that can outlive the source
//! - You want to pass tokens across thread boundaries without lifetimes

use core::sync::atomic::{AtomicBool, Ordering};

use crate::{Stop, StopReason};

/// A cancellation source backed by an `AtomicBool`.
///
/// This is a zero-allocation cancellation primitive. The source owns the
/// atomic and can issue tokens that borrow from it.
///
/// # Example
///
/// ```rust
/// use enough::{AtomicStop, Stop};
///
/// let source = AtomicStop::new();
/// let token = source.token();
///
/// // Check in your operation
/// assert!(!token.should_stop());
///
/// // Cancel when needed
/// source.cancel();
/// assert!(token.should_stop());
/// ```
#[derive(Debug)]
pub struct AtomicStop {
    cancelled: AtomicBool,
}

impl AtomicStop {
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

    /// Cancel all tokens derived from this source.
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

    /// Get a token that can be passed to operations.
    ///
    /// The token borrows from this source, so it cannot outlive it.
    /// For owned tokens, use [`ArcStop`](crate::ArcStop).
    #[inline]
    pub fn token(&self) -> AtomicToken<'_> {
        AtomicToken {
            cancelled: &self.cancelled,
        }
    }
}

impl Default for AtomicStop {
    fn default() -> Self {
        Self::new()
    }
}

impl Stop for AtomicStop {
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

/// A borrowed cancellation token.
///
/// This is a lightweight reference to an [`AtomicStop`]. It can only check
/// for cancellation - it cannot trigger it.
///
/// # Example
///
/// ```rust
/// use enough::{AtomicStop, Stop};
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
/// let source = AtomicStop::new();
/// process(&[0u8; 1000], source.token());
/// ```
#[derive(Debug, Clone, Copy)]
pub struct AtomicToken<'a> {
    cancelled: &'a AtomicBool,
}

impl Stop for AtomicToken<'_> {
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
    fn atomic_stop_basic() {
        let source = AtomicStop::new();
        assert!(!source.is_cancelled());
        assert!(!source.should_stop());
        assert!(source.check().is_ok());

        source.cancel();

        assert!(source.is_cancelled());
        assert!(source.should_stop());
        assert_eq!(source.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn atomic_stop_cancelled_constructor() {
        let source = AtomicStop::cancelled();
        assert!(source.is_cancelled());
        assert!(source.should_stop());
    }

    #[test]
    fn atomic_token_basic() {
        let source = AtomicStop::new();
        let token = source.token();

        assert!(!token.should_stop());
        assert!(token.check().is_ok());

        source.cancel();

        assert!(token.should_stop());
        assert_eq!(token.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn atomic_token_is_copy() {
        let source = AtomicStop::new();
        let t1 = source.token();
        let t2 = t1; // Copy
        let _ = t1; // Still valid
        let _ = t2;
    }

    #[test]
    fn atomic_stop_is_default() {
        let source: AtomicStop = Default::default();
        assert!(!source.is_cancelled());
    }

    #[test]
    fn atomic_stop_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AtomicStop>();
        assert_send_sync::<AtomicToken<'_>>();
    }

    #[test]
    fn cancel_is_idempotent() {
        let source = AtomicStop::new();
        source.cancel();
        source.cancel();
        source.cancel();
        assert!(source.is_cancelled());
    }
}

//! Arc-based cancellation primitives.
//!
//! This module provides owned, cloneable cancellation tokens using `Arc<AtomicBool>`.
//! Requires the `alloc` feature.
//!
//! # Overview
//!
//! - [`ArcStop`] - A cancellation source that can create owned tokens
//! - [`ArcToken`] - An owned token that can outlive its source
//!
//! # Example
//!
//! ```rust
//! use enough::{ArcStop, Stop};
//!
//! let source = ArcStop::new();
//! let token = source.token();
//!
//! // Token is owned and cloneable
//! let token2 = token.clone();
//!
//! assert!(!token.should_stop());
//!
//! source.cancel();
//! assert!(token.should_stop());
//! assert!(token2.should_stop());
//! ```
//!
//! # When to Use
//!
//! Use `ArcStop`/`ArcToken` when:
//! - You need owned tokens without lifetime constraints
//! - You want to pass tokens across thread boundaries
//! - Tokens may outlive the source
//!
//! Use [`AtomicStop`](crate::AtomicStop)/[`AtomicToken`](crate::AtomicToken) when:
//! - You need zero-allocation cancellation
//! - The source always outlives tokens

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{Stop, StopReason};

/// A cancellation source backed by `Arc<AtomicBool>`.
///
/// This source can create owned tokens that share the cancellation state.
/// Both source and tokens hold a reference to the same atomic flag.
///
/// # Example
///
/// ```rust
/// use enough::{ArcStop, Stop};
///
/// let source = ArcStop::new();
/// let token = source.token();
///
/// // Token is owned and can be moved to another thread
/// std::thread::spawn(move || {
///     while !token.should_stop() {
///         // do work
///         break;
///     }
/// }).join().unwrap();
///
/// source.cancel();
/// ```
#[derive(Debug, Clone)]
pub struct ArcStop {
    cancelled: Arc<AtomicBool>,
}

impl ArcStop {
    /// Create a new cancellation source.
    #[inline]
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Create a source that is already cancelled.
    ///
    /// Useful for testing or when you want to signal immediate stop.
    #[inline]
    pub fn cancelled() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(true)),
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

    /// Get an owned token that can be passed to operations.
    ///
    /// The token shares the cancellation state with the source and
    /// can outlive it.
    #[inline]
    pub fn token(&self) -> ArcToken {
        ArcToken {
            cancelled: Arc::clone(&self.cancelled),
        }
    }
}

impl Default for ArcStop {
    fn default() -> Self {
        Self::new()
    }
}

impl Stop for ArcStop {
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

/// An owned cancellation token.
///
/// This token shares the cancellation state with its [`ArcStop`] source.
/// It can only check for cancellation - it cannot trigger it.
///
/// # Example
///
/// ```rust
/// use enough::{ArcStop, ArcToken, Stop};
///
/// fn process(data: &[u8], stop: ArcToken) {
///     for (i, chunk) in data.chunks(100).enumerate() {
///         if i % 10 == 0 && stop.should_stop() {
///             return;
///         }
///         // process chunk...
///     }
/// }
///
/// let source = ArcStop::new();
/// process(&[0u8; 1000], source.token());
/// ```
#[derive(Debug, Clone)]
pub struct ArcToken {
    cancelled: Arc<AtomicBool>,
}

impl Stop for ArcToken {
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
    fn arc_stop_basic() {
        let source = ArcStop::new();
        assert!(!source.is_cancelled());
        assert!(!source.should_stop());
        assert!(source.check().is_ok());

        source.cancel();

        assert!(source.is_cancelled());
        assert!(source.should_stop());
        assert_eq!(source.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn arc_stop_cancelled_constructor() {
        let source = ArcStop::cancelled();
        assert!(source.is_cancelled());
        assert!(source.should_stop());
    }

    #[test]
    fn arc_token_basic() {
        let source = ArcStop::new();
        let token = source.token();

        assert!(!token.should_stop());
        assert!(token.check().is_ok());

        source.cancel();

        assert!(token.should_stop());
        assert_eq!(token.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn arc_token_is_clone() {
        let source = ArcStop::new();
        let t1 = source.token();
        let t2 = t1.clone();
        let t3 = t1.clone();

        source.cancel();

        assert!(t1.should_stop());
        assert!(t2.should_stop());
        assert!(t3.should_stop());
    }

    #[test]
    fn arc_stop_is_clone() {
        let source1 = ArcStop::new();
        let source2 = source1.clone();

        source1.cancel();

        // Both sources share state
        assert!(source1.is_cancelled());
        assert!(source2.is_cancelled());
    }

    #[test]
    fn arc_stop_is_default() {
        let source: ArcStop = Default::default();
        assert!(!source.is_cancelled());
    }

    #[test]
    fn arc_stop_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ArcStop>();
        assert_send_sync::<ArcToken>();
    }

    #[test]
    fn token_can_outlive_source() {
        let token = {
            let source = ArcStop::new();
            source.token()
        };
        // Source is dropped, but token still works
        assert!(!token.should_stop());
    }

    #[test]
    fn cancel_after_source_clone() {
        let source1 = ArcStop::new();
        let source2 = source1.clone();
        let token = source1.token();

        // Cancel via clone
        source2.cancel();

        assert!(token.should_stop());
        assert!(source1.is_cancelled());
    }
}

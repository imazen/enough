//! Combinator for combining multiple stop sources.
//!
//! This module provides [`OrStop`], which combines two stop sources into one
//! that stops when either source stops.
//!
//! # Example
//!
//! ```rust
//! use enough::{AtomicStop, OrStop, Stop};
//!
//! let source_a = AtomicStop::new();
//! let source_b = AtomicStop::new();
//!
//! // Combine: stop if either stops
//! let combined = OrStop::new(source_a.token(), source_b.token());
//!
//! assert!(!combined.should_stop());
//!
//! source_a.cancel();
//! assert!(combined.should_stop());
//! ```

use crate::{Stop, StopReason};

/// Combines two [`Stop`] implementations.
///
/// The combined stop will trigger when either source stops.
///
/// # Example
///
/// ```rust
/// use enough::{AtomicStop, OrStop, Stop};
///
/// let timeout_source = AtomicStop::new();
/// let cancel_source = AtomicStop::new();
///
/// let combined = OrStop::new(timeout_source.token(), cancel_source.token());
///
/// // Not stopped yet
/// assert!(!combined.should_stop());
///
/// // Either source can trigger stop
/// cancel_source.cancel();
/// assert!(combined.should_stop());
/// ```
#[derive(Debug, Clone, Copy)]
pub struct OrStop<A, B> {
    a: A,
    b: B,
}

impl<A, B> OrStop<A, B> {
    /// Create a new combined stop that triggers when either source stops.
    #[inline]
    pub fn new(a: A, b: B) -> Self {
        Self { a, b }
    }

    /// Get a reference to the first stop source.
    #[inline]
    pub fn first(&self) -> &A {
        &self.a
    }

    /// Get a reference to the second stop source.
    #[inline]
    pub fn second(&self) -> &B {
        &self.b
    }

    /// Decompose into the two inner stop sources.
    #[inline]
    pub fn into_inner(self) -> (A, B) {
        (self.a, self.b)
    }
}

impl<A: Stop, B: Stop> Stop for OrStop<A, B> {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        self.a.check()?;
        self.b.check()
    }

    #[inline]
    fn should_stop(&self) -> bool {
        self.a.should_stop() || self.b.should_stop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AtomicStop, Never};

    #[test]
    fn or_stop_neither() {
        let a = AtomicStop::new();
        let b = AtomicStop::new();
        let combined = OrStop::new(a.token(), b.token());

        assert!(!combined.should_stop());
        assert!(combined.check().is_ok());
    }

    #[test]
    fn or_stop_first() {
        let a = AtomicStop::new();
        let b = AtomicStop::new();
        let combined = OrStop::new(a.token(), b.token());

        a.cancel();

        assert!(combined.should_stop());
        assert_eq!(combined.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn or_stop_second() {
        let a = AtomicStop::new();
        let b = AtomicStop::new();
        let combined = OrStop::new(a.token(), b.token());

        b.cancel();

        assert!(combined.should_stop());
        assert_eq!(combined.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn or_stop_both() {
        let a = AtomicStop::new();
        let b = AtomicStop::new();
        let combined = OrStop::new(a.token(), b.token());

        a.cancel();
        b.cancel();

        assert!(combined.should_stop());
    }

    #[test]
    fn or_stop_chain() {
        let a = AtomicStop::new();
        let b = AtomicStop::new();
        let c = AtomicStop::new();

        let combined = OrStop::new(OrStop::new(a.token(), b.token()), c.token());

        assert!(!combined.should_stop());

        c.cancel();
        assert!(combined.should_stop());
    }

    #[test]
    fn or_stop_with_never() {
        let source = AtomicStop::new();
        let combined = OrStop::new(Never, source.token());

        assert!(!combined.should_stop());

        source.cancel();
        assert!(combined.should_stop());
    }

    #[test]
    fn or_stop_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<OrStop<crate::AtomicToken<'_>, crate::AtomicToken<'_>>>();
    }

    #[test]
    fn or_stop_accessors() {
        let a = AtomicStop::new();
        let b = AtomicStop::new();
        let combined = OrStop::new(a.token(), b.token());

        assert!(!combined.first().should_stop());
        assert!(!combined.second().should_stop());

        a.cancel();
        assert!(combined.first().should_stop());

        let (first, second) = combined.into_inner();
        assert!(first.should_stop());
        assert!(!second.should_stop());
    }

    #[test]
    fn or_stop_is_clone() {
        let a = AtomicStop::new();
        let b = AtomicStop::new();
        let combined = OrStop::new(a.token(), b.token());
        let combined2 = combined.clone();

        a.cancel();
        assert!(combined.should_stop());
        assert!(combined2.should_stop());
    }

    #[test]
    fn or_stop_is_copy() {
        let a = AtomicStop::new();
        let b = AtomicStop::new();
        let combined = OrStop::new(a.token(), b.token());
        let combined2 = combined; // Copy
        let _ = combined; // Still valid

        assert!(!combined2.should_stop());
    }
}

//! Hierarchical cancellation.
//!
//! This module provides cancellation sources that inherit from a parent.
//! When a parent is cancelled, all children are also cancelled. Children
//! can also be cancelled independently without affecting siblings or parents.
//!
//! # Overview
//!
//! - [`ChildSource`] - A cancellation source with a parent
//! - [`ChildToken`] - A token from a child source
//!
//! # Example
//!
//! ```rust
//! use enough::{ArcStop, Stop};
//! use enough::children::ChildSource;
//!
//! let parent = ArcStop::new();
//! let child_a = ChildSource::new(parent.token());
//! let child_b = ChildSource::new(parent.token());
//!
//! // Children can be cancelled independently
//! child_a.cancel();
//! assert!(child_a.is_cancelled());
//! assert!(!child_b.is_cancelled());
//!
//! // Parent cancellation propagates to all children
//! parent.cancel();
//! assert!(child_b.is_cancelled());
//! ```
//!
//! # Grandchildren
//!
//! Children can have their own children, creating a cancellation tree:
//!
//! ```rust
//! use enough::{ArcStop, Stop};
//! use enough::children::ChildSource;
//!
//! let grandparent = ArcStop::new();
//! let parent = ChildSource::new(grandparent.token());
//! let child = parent.child();
//!
//! // Grandparent cancellation propagates through the tree
//! grandparent.cancel();
//! assert!(parent.is_cancelled());
//! assert!(child.is_cancelled());
//! ```

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{BoxStop, Stop, StopReason};

/// Inner state for a child cancellation source.
struct ChildInner {
    /// This child's own cancellation flag.
    self_cancelled: AtomicBool,
    /// Parent to check for inherited cancellation.
    parent: BoxStop,
}

impl std::fmt::Debug for ChildInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildInner")
            .field("self_cancelled", &self.self_cancelled)
            .field("parent", &"<BoxStop>")
            .finish()
    }
}

/// A child cancellation source that inherits from a parent.
///
/// When the parent is cancelled, the child is also cancelled.
/// The child can also be cancelled independently without affecting the parent.
///
/// # Example
///
/// ```rust
/// use enough::{ArcStop, Stop};
/// use enough::children::ChildSource;
///
/// let parent = ArcStop::new();
/// let child = ChildSource::new(parent.token());
///
/// // Child can be cancelled independently
/// child.cancel();
/// assert!(child.is_cancelled());
/// assert!(!parent.is_cancelled());
///
/// // Or parent cancellation propagates to children
/// let child2 = ChildSource::new(parent.token());
/// parent.cancel();
/// assert!(child2.is_cancelled());
/// ```
#[derive(Debug, Clone)]
pub struct ChildSource {
    inner: Arc<ChildInner>,
}

impl ChildSource {
    /// Create a new child cancellation source from a parent.
    ///
    /// The child will be cancelled if either:
    /// - [`cancel()`](Self::cancel) is called on this child
    /// - The parent is cancelled
    #[inline]
    pub fn new<T: Stop + 'static>(parent: T) -> Self {
        Self {
            inner: Arc::new(ChildInner {
                self_cancelled: AtomicBool::new(false),
                parent: BoxStop::new(parent),
            }),
        }
    }

    /// Cancel this child source.
    ///
    /// This does NOT affect the parent or siblings.
    #[inline]
    pub fn cancel(&self) {
        self.inner.self_cancelled.store(true, Ordering::Relaxed);
    }

    /// Check if this child was cancelled (either directly or via parent).
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.inner.self_cancelled.load(Ordering::Relaxed) || self.inner.parent.should_stop()
    }

    /// Get a token for this child source.
    #[inline]
    pub fn token(&self) -> ChildToken {
        ChildToken {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Create a grandchild source from this child.
    ///
    /// The grandchild will be cancelled if either this child or any
    /// ancestor is cancelled.
    #[inline]
    pub fn child(&self) -> ChildSource {
        ChildSource::new(self.token())
    }
}

impl Stop for ChildSource {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        if self.inner.self_cancelled.load(Ordering::Relaxed) {
            return Err(StopReason::Cancelled);
        }
        self.inner.parent.check()
    }

    #[inline]
    fn should_stop(&self) -> bool {
        self.is_cancelled()
    }
}

/// A token from a child cancellation source.
///
/// This token checks both its own cancellation state and its parent chain.
#[derive(Debug, Clone)]
pub struct ChildToken {
    inner: Arc<ChildInner>,
}

impl Stop for ChildToken {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        // Check self first
        if self.inner.self_cancelled.load(Ordering::Relaxed) {
            return Err(StopReason::Cancelled);
        }
        // Check parent chain
        self.inner.parent.check()
    }

    #[inline]
    fn should_stop(&self) -> bool {
        self.inner.self_cancelled.load(Ordering::Relaxed) || self.inner.parent.should_stop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ArcStop;

    #[test]
    fn child_inherits_parent() {
        let parent = ArcStop::new();
        let child = ChildSource::new(parent.token());

        assert!(!child.is_cancelled());

        parent.cancel();

        assert!(child.is_cancelled());
    }

    #[test]
    fn child_cancel_independent() {
        let parent = ArcStop::new();
        let child = ChildSource::new(parent.token());

        child.cancel();

        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());
    }

    #[test]
    fn siblings_independent() {
        let parent = ArcStop::new();
        let child_a = ChildSource::new(parent.token());
        let child_b = ChildSource::new(parent.token());

        child_a.cancel();

        assert!(child_a.is_cancelled());
        assert!(!child_b.is_cancelled());

        parent.cancel();
        assert!(child_b.is_cancelled());
    }

    #[test]
    fn grandchild() {
        let grandparent = ArcStop::new();
        let parent = ChildSource::new(grandparent.token());
        let child = parent.child();

        assert!(!child.is_cancelled());

        grandparent.cancel();
        assert!(child.is_cancelled());
    }

    #[test]
    fn child_token_basic() {
        let parent = ArcStop::new();
        let child = ChildSource::new(parent.token());
        let token = child.token();

        assert!(!token.should_stop());

        child.cancel();

        assert!(token.should_stop());
    }

    #[test]
    fn child_token_inherits_parent() {
        let parent = ArcStop::new();
        let child = ChildSource::new(parent.token());
        let token = child.token();

        parent.cancel();

        assert!(token.should_stop());
    }

    #[test]
    fn child_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ChildSource>();
        assert_send_sync::<ChildToken>();
    }

    #[test]
    fn child_source_impl_stop() {
        let parent = ArcStop::new();
        let child = ChildSource::new(parent.token());

        assert!(child.check().is_ok());

        parent.cancel();

        assert_eq!(child.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn three_generations() {
        let g1 = ArcStop::new();
        let g2 = ChildSource::new(g1.token());
        let g3 = g2.child();

        assert!(!g3.is_cancelled());

        // Cancel middle generation
        g2.cancel();

        assert!(!g1.is_cancelled());
        assert!(g2.is_cancelled());
        assert!(g3.is_cancelled());
    }
}

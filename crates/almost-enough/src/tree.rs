//! Hierarchical cancellation with tree structure.
//!
//! [`ChildStopper`] provides cancellation that can form parent-child relationships.
//! When a parent is cancelled, all children are also cancelled. Children can be
//! cancelled independently without affecting siblings or parents.
//!
//! # Overview
//!
//! - [`ChildStopper::new()`] - Create a root (no parent)
//! - [`ChildStopper::with_parent()`] - Create a child of any `Stop` implementation
//! - [`tree.child()`](ChildStopper::child) - Create a child of this tree node
//!
//! # Example
//!
//! ```rust
//! use almost_enough::{ChildStopper, Stop};
//!
//! let parent = ChildStopper::new();
//! let child_a = parent.child();
//! let child_b = parent.child();
//!
//! // Children can be cancelled independently
//! child_a.cancel();
//! assert!(child_a.should_stop());
//! assert!(!child_b.should_stop());
//!
//! // Parent cancellation propagates to all children
//! parent.cancel();
//! assert!(child_b.should_stop());
//! ```
//!
//! # Grandchildren
//!
//! Children can have their own children, creating a cancellation tree:
//!
//! ```rust
//! use almost_enough::{ChildStopper, Stop};
//!
//! let grandparent = ChildStopper::new();
//! let parent = grandparent.child();
//! let child = parent.child();
//!
//! // Grandparent cancellation propagates through the tree
//! grandparent.cancel();
//! assert!(parent.should_stop());
//! assert!(child.should_stop());
//! ```
//!
//! # With Other Stop Types
//!
//! You can create a `ChildStopper` as a child of any `Stop` implementation:
//!
//! ```rust
//! use almost_enough::{Stopper, ChildStopper, Stop};
//!
//! let root = Stopper::new();
//! let child = ChildStopper::with_parent(root.clone());
//!
//! root.cancel();
//! assert!(child.should_stop());
//! ```

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{BoxedStop, Stop, StopReason};

/// Inner state for a tree node.
struct TreeInner {
    /// This node's own cancellation flag.
    self_cancelled: AtomicBool,
    /// Parent to check for inherited cancellation (None for root).
    parent: Option<BoxedStop>,
}

impl std::fmt::Debug for TreeInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TreeInner")
            .field("self_cancelled", &self.self_cancelled)
            .field("parent", &self.parent.as_ref().map(|_| "<BoxedStop>"))
            .finish()
    }
}

/// A cancellation primitive with tree-structured parent-child relationships.
///
/// `ChildStopper` uses a unified clone model: clone to share, any clone can cancel.
/// When cancelled, it does NOT affect its parent or siblings - only this node
/// and any of its children.
///
/// # Example
///
/// ```rust
/// use almost_enough::{ChildStopper, Stop};
///
/// let parent = ChildStopper::new();
/// let child = parent.child();
///
/// // Clone to share across threads
/// let child_clone = child.clone();
///
/// // Any clone can cancel
/// child_clone.cancel();
/// assert!(child.should_stop());
///
/// // Parent is not affected
/// assert!(!parent.should_stop());
/// ```
///
/// # Performance
///
/// - Size: 8 bytes (one pointer)
/// - `check()`: ~5-20ns depending on tree depth (walks parent chain)
/// - Root nodes: no parent check, similar to `Stopper`
#[derive(Debug, Clone)]
pub struct ChildStopper {
    inner: Arc<TreeInner>,
}

impl ChildStopper {
    /// Create a new root tree node (no parent).
    ///
    /// This creates a tree root that can have children added via [`child()`](Self::child).
    ///
    /// # Example
    ///
    /// ```rust
    /// use almost_enough::{ChildStopper, Stop};
    ///
    /// let root = ChildStopper::new();
    /// let child = root.child();
    ///
    /// root.cancel();
    /// assert!(child.should_stop());
    /// ```
    #[inline]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(TreeInner {
                self_cancelled: AtomicBool::new(false),
                parent: None,
            }),
        }
    }

    /// Create a new tree node with a parent.
    ///
    /// The child will stop if either:
    /// - [`cancel()`](Self::cancel) is called on this node (or any clone)
    /// - Any ancestor in the parent chain is cancelled
    ///
    /// # Example
    ///
    /// ```rust
    /// use almost_enough::{Stopper, ChildStopper, Stop};
    ///
    /// let root = Stopper::new();
    /// let child = ChildStopper::with_parent(root.clone());
    ///
    /// root.cancel();
    /// assert!(child.should_stop());
    /// ```
    #[inline]
    pub fn with_parent<T: Stop + 'static>(parent: T) -> Self {
        Self {
            inner: Arc::new(TreeInner {
                self_cancelled: AtomicBool::new(false),
                parent: Some(BoxedStop::new(parent)),
            }),
        }
    }

    /// Create a child of this tree node.
    ///
    /// The child will stop if either this node or any ancestor is cancelled.
    /// Cancelling the child does NOT affect this node.
    ///
    /// # Example
    ///
    /// ```rust
    /// use almost_enough::{ChildStopper, Stop};
    ///
    /// let parent = ChildStopper::new();
    /// let child = parent.child();
    /// let grandchild = child.child();
    ///
    /// child.cancel();
    /// assert!(!parent.should_stop());  // Parent unaffected
    /// assert!(child.should_stop());
    /// assert!(grandchild.should_stop());  // Inherits from parent
    /// ```
    #[inline]
    pub fn child(&self) -> ChildStopper {
        ChildStopper::with_parent(self.clone())
    }

    /// Cancel this node (and all its children).
    ///
    /// This does NOT affect the parent or siblings.
    #[inline]
    pub fn cancel(&self) {
        self.inner.self_cancelled.store(true, Ordering::Relaxed);
    }

    /// Check if this node is cancelled (either directly or via ancestor).
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        if self.inner.self_cancelled.load(Ordering::Relaxed) {
            return true;
        }
        if let Some(ref parent) = self.inner.parent {
            parent.should_stop()
        } else {
            false
        }
    }
}

impl Default for ChildStopper {
    fn default() -> Self {
        Self::new()
    }
}

impl Stop for ChildStopper {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        if self.inner.self_cancelled.load(Ordering::Relaxed) {
            return Err(StopReason::Cancelled);
        }
        if let Some(ref parent) = self.inner.parent {
            parent.check()
        } else {
            Ok(())
        }
    }

    #[inline]
    fn should_stop(&self) -> bool {
        self.is_cancelled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Stopper;

    #[test]
    fn tree_root_basic() {
        let root = ChildStopper::new();
        assert!(!root.is_cancelled());
        assert!(!root.should_stop());
        assert!(root.check().is_ok());

        root.cancel();

        assert!(root.is_cancelled());
        assert!(root.should_stop());
        assert_eq!(root.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn tree_child_inherits_parent() {
        let parent = ChildStopper::new();
        let child = parent.child();

        assert!(!child.is_cancelled());

        parent.cancel();

        assert!(child.is_cancelled());
    }

    #[test]
    fn tree_child_cancel_independent() {
        let parent = ChildStopper::new();
        let child = parent.child();

        child.cancel();

        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());
    }

    #[test]
    fn tree_siblings_independent() {
        let parent = ChildStopper::new();
        let child_a = parent.child();
        let child_b = parent.child();

        child_a.cancel();

        assert!(child_a.is_cancelled());
        assert!(!child_b.is_cancelled());

        parent.cancel();
        assert!(child_b.is_cancelled());
    }

    #[test]
    fn tree_grandchild() {
        let grandparent = ChildStopper::new();
        let parent = grandparent.child();
        let child = parent.child();

        assert!(!child.is_cancelled());

        grandparent.cancel();
        assert!(child.is_cancelled());
    }

    #[test]
    fn tree_three_generations() {
        let g1 = ChildStopper::new();
        let g2 = g1.child();
        let g3 = g2.child();

        assert!(!g3.is_cancelled());

        // Cancel middle generation
        g2.cancel();

        assert!(!g1.is_cancelled());
        assert!(g2.is_cancelled());
        assert!(g3.is_cancelled());
    }

    #[test]
    fn tree_with_stopper_parent() {
        let root = Stopper::new();
        let child = ChildStopper::with_parent(root.clone());

        assert!(!child.is_cancelled());

        root.cancel();

        assert!(child.is_cancelled());
    }

    #[test]
    fn tree_clone_shares_state() {
        let t1 = ChildStopper::new();
        let t2 = t1.clone();

        t2.cancel();

        assert!(t1.is_cancelled());
        assert!(t2.is_cancelled());
    }

    #[test]
    fn tree_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ChildStopper>();
    }

    #[test]
    fn tree_is_default() {
        let t: ChildStopper = Default::default();
        assert!(!t.is_cancelled());
    }
}

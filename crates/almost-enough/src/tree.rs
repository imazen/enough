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
use core::any::{Any, TypeId};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{Stop, StopReason, StopToken, Unstoppable};

/// Inner state for a tree node.
struct TreeInner {
    /// This node's own cancellation flag.
    self_cancelled: AtomicBool,
    /// The parent node, if it is another `ChildStopper`: checked directly, so
    /// a chain of them is a loop of atomic loads with no vtable call per level.
    parent: Option<Arc<TreeInner>>,
    /// Any other stop above the top of the chain, checked once there.
    /// `Unstoppable` (stored as nothing) when there is none.
    above: StopToken,
}

impl TreeInner {
    fn root(above: StopToken) -> Self {
        Self {
            self_cancelled: AtomicBool::new(false),
            parent: None,
            above,
        }
    }

    /// Walk this node and its ancestors, stopping at the first cancelled one.
    #[inline]
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        let mut node = self;
        loop {
            if node.self_cancelled.load(Ordering::Relaxed) {
                return Err(StopReason::Cancelled);
            }
            match &node.parent {
                Some(parent) => node = parent,
                None => return node.above.check(),
            }
        }
    }

    #[inline]
    #[track_caller]
    fn should_stop(&self) -> bool {
        let mut node = self;
        loop {
            if node.self_cancelled.load(Ordering::Relaxed) {
                return true;
            }
            match &node.parent {
                Some(parent) => node = parent,
                None => return node.above.should_stop(),
            }
        }
    }
}

impl core::fmt::Debug for TreeInner {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let parent = if self.parent.is_some() {
            Some("<ChildStopper>")
        } else if self.above.may_stop() {
            Some("<StopToken>")
        } else {
            None
        };
        f.debug_struct("TreeInner")
            .field("self_cancelled", &self.self_cancelled)
            .field("parent", &parent)
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
/// - `check()`: one atomic load per level of the tree. A parent that is
///   another `ChildStopper` is checked directly; any other parent is checked
///   once, at the top of the chain, as a [`StopToken`] would check it.
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
            inner: Arc::new(TreeInner::root(StopToken::new(Unstoppable))),
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
        if TypeId::of::<T>() == TypeId::of::<ChildStopper>() {
            let any_ref: &dyn Any = &parent;
            return any_ref.downcast_ref::<ChildStopper>().unwrap().child();
        }
        Self {
            inner: Arc::new(TreeInner::root(StopToken::new(parent))),
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
        Self {
            inner: Arc::new(TreeInner {
                self_cancelled: AtomicBool::new(false),
                parent: Some(Arc::clone(&self.inner)),
                above: StopToken::new(Unstoppable),
            }),
        }
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
        self.inner.should_stop()
    }
}

impl Default for ChildStopper {
    fn default() -> Self {
        Self::new()
    }
}

impl Stop for ChildStopper {
    #[inline]
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.inner.check()
    }

    #[inline]
    #[track_caller]
    fn should_stop(&self) -> bool {
        self.inner.should_stop()
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

    #[test]
    fn child_stopper_parents_are_walked_directly() {
        let root = ChildStopper::new();
        assert!(root.inner.parent.is_none() && !root.inner.above.may_stop());
        let child = root.child();
        assert!(Arc::ptr_eq(
            child.inner.parent.as_ref().unwrap(),
            &root.inner
        ));
        let adopted = ChildStopper::with_parent(child.clone());
        assert!(Arc::ptr_eq(
            adopted.inner.parent.as_ref().unwrap(),
            &child.inner
        ));
        assert!(!adopted.inner.above.may_stop());
        let other = ChildStopper::with_parent(Stopper::new());
        assert!(other.inner.parent.is_none() && other.inner.above.may_stop());
    }

    #[test]
    fn a_deep_chain_stops_below_the_cancelled_node_only() {
        let root = ChildStopper::with_parent(Stopper::new());
        let mut chain = alloc::vec![root];
        for _ in 0..100 {
            let next = chain.last().unwrap().child();
            chain.push(next);
        }
        assert!(chain.iter().all(|node| node.check().is_ok()));
        chain[40].cancel();
        for (depth, node) in chain.iter().enumerate() {
            assert_eq!(node.should_stop(), depth >= 40, "depth {depth}");
            assert_eq!(node.is_cancelled(), depth >= 40, "depth {depth}");
            let expected = if depth >= 40 {
                Err(StopReason::Cancelled)
            } else {
                Ok(())
            };
            assert_eq!(node.check(), expected, "depth {depth}");
        }
    }

    #[test]
    fn a_stopper_at_the_top_stops_the_whole_chain() {
        let stopper = Stopper::new();
        let leaf = ChildStopper::with_parent(stopper.clone()).child().child();
        assert!(!leaf.should_stop());
        stopper.cancel();
        assert!(leaf.should_stop());
        assert_eq!(leaf.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn the_top_stops_reason_reaches_the_leaf() {
        struct TimedOut;
        impl Stop for TimedOut {
            fn check(&self) -> Result<(), StopReason> {
                Err(StopReason::TimedOut)
            }
        }
        let leaf = ChildStopper::with_parent(TimedOut).child().child();
        assert_eq!(leaf.check(), Err(StopReason::TimedOut));
        assert!(leaf.should_stop());
    }

    #[test]
    fn debug_names_the_parent_kind() {
        let root = ChildStopper::new();
        assert!(alloc::format!("{root:?}").contains("parent: None"));
        assert!(alloc::format!("{:?}", root.child()).contains("<ChildStopper>"));
        let adopted = ChildStopper::with_parent(Stopper::new());
        assert!(alloc::format!("{adopted:?}").contains("<StopToken>"));
    }
}

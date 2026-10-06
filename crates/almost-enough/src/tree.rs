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
use core::sync::atomic::{AtomicU8, Ordering};

use crate::{Stop, StopReason, StopToken, Unstoppable};

/// `TreeInner::state`: not cancelled, with nothing above but parent nodes.
const RUNNING: u8 = 0;
/// `TreeInner::state`: this node was cancelled.
const CANCELLED: u8 = 1;
/// `TreeInner::state`: not cancelled, and `above` must be checked.
const ABOVE: u8 = 2;

// The checks test `state != RUNNING`, then `state & CANCELLED`; a `match` on
// the state compiles longer on x86-64.
const _: () = assert!(RUNNING == 0 && CANCELLED != 0 && ABOVE != 0 && ABOVE & CANCELLED == 0);

/// Inner state for a tree node.
///
/// A check walks the chain with no vtable call: each level loads and tests
/// the state byte, then loads and tests the parent pointer.
struct TreeInner {
    /// `RUNNING`, `CANCELLED`, or `ABOVE`, which `cancel()` also overwrites.
    state: AtomicU8,
    /// The parent node, if it is another `ChildStopper`.
    parent: Option<Arc<TreeInner>>,
    /// Any other parent, checked only in state `ABOVE`; `Unstoppable`
    /// (stored as nothing) otherwise.
    above: StopToken,
}

impl TreeInner {
    fn new(parent: Option<Arc<TreeInner>>, above: StopToken) -> Self {
        let state = if above.may_stop() { ABOVE } else { RUNNING };
        Self {
            state: AtomicU8::new(state),
            parent,
            above,
        }
    }

    /// Check this node, then walk its ancestors. The first level is peeled
    /// out of the loop so a root returns without a jump.
    #[inline]
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        let state = self.state.load(Ordering::Relaxed);
        if state != RUNNING {
            if state & CANCELLED != 0 {
                return Err(StopReason::Cancelled);
            }
            return self.above.check();
        }
        match &self.parent {
            None => Ok(()),
            Some(parent) => parent.check_ancestors(),
        }
    }

    #[inline]
    #[track_caller]
    fn check_ancestors(&self) -> Result<(), StopReason> {
        let mut node = self;
        loop {
            let state = node.state.load(Ordering::Relaxed);
            if state != RUNNING {
                if state & CANCELLED != 0 {
                    return Err(StopReason::Cancelled);
                }
                return node.above.check();
            }
            match &node.parent {
                Some(parent) => node = parent,
                None => return Ok(()),
            }
        }
    }

    #[inline]
    #[track_caller]
    fn should_stop(&self) -> bool {
        let state = self.state.load(Ordering::Relaxed);
        if state != RUNNING {
            return state & CANCELLED != 0 || self.above.should_stop();
        }
        match &self.parent {
            None => false,
            Some(parent) => parent.should_stop_ancestors(),
        }
    }

    #[inline]
    #[track_caller]
    fn should_stop_ancestors(&self) -> bool {
        let mut node = self;
        loop {
            let state = node.state.load(Ordering::Relaxed);
            if state != RUNNING {
                return state & CANCELLED != 0 || node.above.should_stop();
            }
            match &node.parent {
                Some(parent) => node = parent,
                None => return false,
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
            .field(
                "self_cancelled",
                &(self.state.load(Ordering::Relaxed) == CANCELLED),
            )
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
/// - `check()`: per level of the tree, a state byte and a parent pointer,
///   with no vtable call. A parent that isn't a `ChildStopper` is checked
///   once, at the top of the chain, as a [`StopToken`] would check it.
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
            inner: Arc::new(TreeInner::new(None, StopToken::new(Unstoppable))),
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
            inner: Arc::new(TreeInner::new(None, StopToken::new(parent))),
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
            inner: Arc::new(TreeInner::new(
                Some(Arc::clone(&self.inner)),
                StopToken::new(Unstoppable),
            )),
        }
    }

    /// Cancel this node (and all its children).
    ///
    /// This does NOT affect the parent or siblings.
    #[inline]
    pub fn cancel(&self) {
        self.inner.state.store(CANCELLED, Ordering::Relaxed);
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
        assert!(root.inner.parent.is_none());
        assert_eq!(root.inner.state.load(Ordering::Relaxed), RUNNING);
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
        let other = ChildStopper::with_parent(Stopper::new());
        assert!(other.inner.parent.is_none());
        assert_eq!(other.inner.state.load(Ordering::Relaxed), ABOVE);
        let never = ChildStopper::with_parent(crate::Unstoppable);
        assert_eq!(never.inner.state.load(Ordering::Relaxed), RUNNING);
    }

    #[test]
    fn cancelling_a_node_under_another_stop_overrides_it() {
        let stopper = Stopper::new();
        let node = ChildStopper::with_parent(stopper.clone());
        let leaf = node.child();
        assert!(!leaf.should_stop());
        node.cancel();
        assert_eq!(node.inner.state.load(Ordering::Relaxed), CANCELLED);
        assert_eq!(leaf.check(), Err(StopReason::Cancelled));
        assert!(!stopper.should_stop());
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
    fn a_child_stopper_inside_a_stop_token_is_checked_above() {
        let root = ChildStopper::new();
        let middle = ChildStopper::with_parent(StopToken::new(root.clone()));
        assert!(middle.inner.parent.is_none());
        assert_eq!(middle.inner.state.load(Ordering::Relaxed), ABOVE);
        let leaf = middle.child();
        assert!(leaf.check().is_ok());
        root.cancel();
        assert_eq!(leaf.check(), Err(StopReason::Cancelled));
        assert!(leaf.should_stop());
    }

    #[test]
    fn a_cancelled_node_reports_cancelled_over_a_timed_out_parent() {
        struct TimedOut;
        impl Stop for TimedOut {
            fn check(&self) -> Result<(), StopReason> {
                Err(StopReason::TimedOut)
            }
        }
        let node = ChildStopper::with_parent(TimedOut);
        node.cancel();
        assert_eq!(node.check(), Err(StopReason::Cancelled));
        assert_eq!(node.child().check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn an_unstoppable_parent_makes_a_root() {
        let node = ChildStopper::with_parent(crate::Unstoppable);
        assert!(node.inner.parent.is_none());
        assert!(!node.inner.above.may_stop());
        assert!(alloc::format!("{node:?}").contains("parent: None"));
        assert!(node.check().is_ok());
        node.cancel();
        assert_eq!(node.check(), Err(StopReason::Cancelled));
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

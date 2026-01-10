//! Child cancellation source - hierarchical cancellation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use enough::{Stop, StopReason};
use smallvec::SmallVec;

use crate::CancellationToken;

/// A cancellation source that inherits from a parent.
///
/// When the parent is cancelled, this child is also cancelled.
/// When this child is cancelled, the parent is unaffected.
///
/// This enables hierarchical cancellation patterns:
///
/// ```text
/// Request (cancel = abort everything)
///   ├── Image A (cancel = skip, continue with B and C)
///   ├── Image B (cancel = skip, continue with C)
///   └── Image C
/// ```
///
/// # Example
///
/// ```rust
/// use enough_std::{CancellationSource, ChildCancellationSource};
/// use enough::Stop;
///
/// let parent = CancellationSource::new();
///
/// let child_a = ChildCancellationSource::new(parent.token());
/// let child_b = ChildCancellationSource::new(parent.token());
///
/// // Cancel child_a only
/// child_a.cancel();
/// assert!(child_a.token().is_stopped());
/// assert!(!child_b.token().is_stopped());
///
/// // Cancel parent - both children stop
/// parent.cancel();
/// assert!(child_b.token().is_stopped());
/// ```
pub struct ChildCancellationSource {
    /// This source's own flag
    own_flag: AtomicBool,
    /// Parent flags to check (in order: immediate parent, grandparent, etc.)
    parent_flags: SmallVec<[*const AtomicBool; 4]>,
}

impl ChildCancellationSource {
    /// Create a new child source that inherits from a parent token.
    pub fn new(parent: CancellationToken) -> Self {
        let mut parent_flags = SmallVec::new();

        // Extract parent's flag if it has one
        if !parent.flag.is_null() {
            parent_flags.push(parent.flag);
        }

        Self {
            own_flag: AtomicBool::new(false),
            parent_flags,
        }
    }

    /// Create a child of another child source.
    ///
    /// The new child inherits all ancestors' cancellation.
    pub fn child(&self) -> ChildCancellationSource {
        let mut parent_flags = SmallVec::new();
        parent_flags.push(&self.own_flag as *const AtomicBool);
        parent_flags.extend(self.parent_flags.iter().copied());

        ChildCancellationSource {
            own_flag: AtomicBool::new(false),
            parent_flags,
        }
    }

    /// Cancel this child (and all its descendants).
    ///
    /// The parent is unaffected.
    #[inline]
    pub fn cancel(&self) {
        self.own_flag.store(true, Ordering::Release);
    }

    /// Check if this child has been cancelled (not including parent).
    #[inline]
    pub fn is_self_cancelled(&self) -> bool {
        self.own_flag.load(Ordering::Acquire)
    }

    /// Check if cancelled (self or any parent).
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        if self.own_flag.load(Ordering::Acquire) {
            return true;
        }
        for &flag in &self.parent_flags {
            // SAFETY: Parent flags are valid as long as parents exist
            if unsafe { (*flag).load(Ordering::Acquire) } {
                return true;
            }
        }
        false
    }

    /// Get a token for this child source.
    pub fn token(&self) -> ChildCancellationToken {
        let mut flags = SmallVec::new();
        flags.push(&self.own_flag as *const AtomicBool);
        flags.extend(self.parent_flags.iter().copied());

        ChildCancellationToken {
            flags,
            deadline: None,
        }
    }

    /// Reset this child's cancellation state.
    ///
    /// Does not affect parent state.
    #[inline]
    pub fn reset(&self) {
        self.own_flag.store(false, Ordering::Release);
    }
}

// SAFETY: AtomicBool is Send + Sync, and parent_flags are only read
unsafe impl Send for ChildCancellationSource {}
unsafe impl Sync for ChildCancellationSource {}

impl std::fmt::Debug for ChildCancellationSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildCancellationSource")
            .field("self_cancelled", &self.is_self_cancelled())
            .field("any_cancelled", &self.is_cancelled())
            .field("parent_count", &self.parent_flags.len())
            .finish()
    }
}

/// Token for a child cancellation source.
///
/// Checks multiple flags (self + all ancestors).
#[derive(Clone)]
pub struct ChildCancellationToken {
    /// Flags to check (self first, then parents)
    flags: SmallVec<[*const AtomicBool; 4]>,
    /// Optional deadline
    deadline: Option<Instant>,
}

// SAFETY: Only reads from AtomicBool pointers
unsafe impl Send for ChildCancellationToken {}
unsafe impl Sync for ChildCancellationToken {}

impl ChildCancellationToken {
    /// Add a timeout to this token.
    pub fn with_timeout(self, duration: std::time::Duration) -> Self {
        self.with_deadline(Instant::now() + duration)
    }

    /// Add a deadline to this token.
    pub fn with_deadline(self, new_deadline: Instant) -> Self {
        let deadline = match self.deadline {
            Some(existing) => Some(existing.min(new_deadline)),
            None => Some(new_deadline),
        };
        Self { deadline, ..self }
    }

    fn is_any_flag_set(&self) -> bool {
        for &flag in &self.flags {
            if !flag.is_null() {
                // SAFETY: Caller guarantees flags are valid
                if unsafe { (*flag).load(Ordering::Acquire) } {
                    return true;
                }
            }
        }
        false
    }

    fn is_deadline_passed(&self) -> bool {
        self.deadline.map(|d| Instant::now() >= d).unwrap_or(false)
    }
}

impl Stop for ChildCancellationToken {
    fn check(&self) -> Result<(), StopReason> {
        if self.is_any_flag_set() {
            return Err(StopReason::Cancelled);
        }
        if self.is_deadline_passed() {
            return Err(StopReason::TimedOut);
        }
        Ok(())
    }

    fn is_stopped(&self) -> bool {
        self.is_any_flag_set() || self.is_deadline_passed()
    }
}

impl std::fmt::Debug for ChildCancellationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildCancellationToken")
            .field("flag_count", &self.flags.len())
            .field("deadline", &self.deadline)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CancellationSource;

    #[test]
    fn child_inherits_parent_cancel() {
        let parent = CancellationSource::new();
        let child = ChildCancellationSource::new(parent.token());

        assert!(!child.is_cancelled());

        parent.cancel();

        assert!(child.is_cancelled());
        assert!(child.token().is_stopped());
    }

    #[test]
    fn child_cancel_does_not_affect_parent() {
        let parent = CancellationSource::new();
        let child = ChildCancellationSource::new(parent.token());

        child.cancel();

        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());
    }

    #[test]
    fn siblings_are_independent() {
        let parent = CancellationSource::new();
        let child_a = ChildCancellationSource::new(parent.token());
        let child_b = ChildCancellationSource::new(parent.token());

        child_a.cancel();

        assert!(child_a.is_cancelled());
        assert!(!child_b.is_cancelled());
    }

    #[test]
    fn grandchild_inherits_all() {
        let grandparent = CancellationSource::new();
        let parent = ChildCancellationSource::new(grandparent.token());
        let child = parent.child();

        assert!(!child.is_cancelled());

        grandparent.cancel();

        assert!(child.is_cancelled());
    }

    #[test]
    fn deep_hierarchy() {
        let root = CancellationSource::new();
        let level1 = ChildCancellationSource::new(root.token());
        let level2 = level1.child();
        let level3 = level2.child();
        let level4 = level3.child();

        assert!(!level4.is_cancelled());

        root.cancel();

        assert!(level4.is_cancelled());
    }

    #[test]
    fn child_reset() {
        let parent = CancellationSource::new();
        let child = ChildCancellationSource::new(parent.token());

        child.cancel();
        assert!(child.is_self_cancelled());

        child.reset();
        assert!(!child.is_self_cancelled());
        assert!(!child.is_cancelled());
    }

    #[test]
    fn child_token_with_timeout() {
        let parent = CancellationSource::new();
        let child = ChildCancellationSource::new(parent.token());
        let token = child
            .token()
            .with_timeout(std::time::Duration::from_millis(10));

        assert!(!token.is_stopped());
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(token.is_stopped());
    }

    #[test]
    fn child_source_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ChildCancellationSource>();
        assert_send_sync::<ChildCancellationToken>();
    }
}

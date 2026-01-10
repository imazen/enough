//! Child cancellation sources for hierarchical cancellation.
//!
//! This module requires the `std` feature.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{CancellationToken, Stop, StopReason};

/// Inner state for a child cancellation source.
struct ChildInner {
    /// This child's own cancellation flag.
    self_cancelled: AtomicBool,
    /// Parent to check for inherited cancellation.
    parent: Box<dyn Stop + Send + Sync>,
}

impl std::fmt::Debug for ChildInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildInner")
            .field("self_cancelled", &self.self_cancelled)
            .field("parent", &"<dyn Stop>")
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
/// use enough::{CancellationSource, ChildCancellationSource, Stop};
///
/// let parent = CancellationSource::new();
/// let child = ChildCancellationSource::new(parent.token());
///
/// // Child can be cancelled independently
/// child.cancel();
/// assert!(child.is_cancelled());
/// assert!(!parent.is_cancelled());
///
/// // Or parent cancellation propagates to children
/// let child2 = ChildCancellationSource::new(parent.token());
/// parent.cancel();
/// assert!(child2.is_cancelled());
/// ```
#[derive(Debug, Clone)]
pub struct ChildCancellationSource {
    inner: Arc<ChildInner>,
}

impl ChildCancellationSource {
    /// Create a new child cancellation source from a parent token.
    #[inline]
    pub fn new(parent: CancellationToken) -> Self {
        Self {
            inner: Arc::new(ChildInner {
                self_cancelled: AtomicBool::new(false),
                parent: Box::new(parent),
            }),
        }
    }

    /// Create a child from any Stop implementation.
    fn from_stop(parent: impl Stop + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(ChildInner {
                self_cancelled: AtomicBool::new(false),
                parent: Box::new(parent),
            }),
        }
    }

    /// Cancel this child source.
    ///
    /// This does NOT affect the parent or siblings.
    #[inline]
    pub fn cancel(&self) {
        self.inner.self_cancelled.store(true, Ordering::Release);
    }

    /// Check if this child was cancelled (either directly or via parent).
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.inner.self_cancelled.load(Ordering::Acquire) || self.inner.parent.is_stopped()
    }

    /// Get a token for this child source.
    #[inline]
    pub fn token(&self) -> ChildCancellationToken {
        ChildCancellationToken {
            inner: Arc::clone(&self.inner),
            deadline: None,
        }
    }

    /// Create a grandchild source from this child.
    ///
    /// The grandchild will be cancelled if either this child or the
    /// grandparent (or any ancestor) is cancelled.
    #[inline]
    pub fn child(&self) -> ChildCancellationSource {
        ChildCancellationSource::from_stop(self.token())
    }
}

/// A token from a child cancellation source.
#[derive(Debug, Clone)]
pub struct ChildCancellationToken {
    inner: Arc<ChildInner>,
    deadline: Option<Instant>,
}

impl ChildCancellationToken {
    /// Add a timeout to this token.
    #[inline]
    pub fn with_timeout(self, duration: Duration) -> Self {
        self.with_deadline(Instant::now() + duration)
    }

    /// Add an absolute deadline to this token.
    #[inline]
    pub fn with_deadline(self, new_deadline: Instant) -> Self {
        let deadline = match self.deadline {
            Some(existing) => Some(existing.min(new_deadline)),
            None => Some(new_deadline),
        };
        Self { deadline, ..self }
    }

    /// Get the deadline, if any.
    #[inline]
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Get the remaining time until deadline.
    #[inline]
    pub fn remaining(&self) -> Option<Duration> {
        self.deadline
            .map(|d| d.saturating_duration_since(Instant::now()))
    }

    #[inline]
    fn is_timed_out(&self) -> bool {
        self.deadline.map(|d| Instant::now() >= d).unwrap_or(false)
    }
}

impl Stop for ChildCancellationToken {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        // Check self first
        if self.inner.self_cancelled.load(Ordering::Acquire) {
            return Err(StopReason::Cancelled);
        }
        // Check parent chain
        if let Err(e) = self.inner.parent.check() {
            return Err(e);
        }
        // Check timeout
        if self.is_timed_out() {
            return Err(StopReason::TimedOut);
        }
        Ok(())
    }

    #[inline]
    fn is_stopped(&self) -> bool {
        self.inner.self_cancelled.load(Ordering::Acquire)
            || self.inner.parent.is_stopped()
            || self.is_timed_out()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CancellationSource;

    #[test]
    fn child_inherits_parent() {
        let parent = CancellationSource::new();
        let child = ChildCancellationSource::new(parent.token());

        assert!(!child.is_cancelled());

        parent.cancel();

        assert!(child.is_cancelled());
    }

    #[test]
    fn child_cancel_independent() {
        let parent = CancellationSource::new();
        let child = ChildCancellationSource::new(parent.token());

        child.cancel();

        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());
    }

    #[test]
    fn siblings_independent() {
        let parent = CancellationSource::new();
        let child_a = ChildCancellationSource::new(parent.token());
        let child_b = ChildCancellationSource::new(parent.token());

        child_a.cancel();

        assert!(child_a.is_cancelled());
        assert!(!child_b.is_cancelled());

        parent.cancel();
        assert!(child_b.is_cancelled());
    }

    #[test]
    fn grandchild() {
        let grandparent = CancellationSource::new();
        let parent = ChildCancellationSource::new(grandparent.token());
        let child = parent.child();

        assert!(!child.is_cancelled());

        grandparent.cancel();
        assert!(child.is_cancelled());
    }

    #[test]
    fn child_token_with_timeout() {
        let parent = CancellationSource::new();
        let child = ChildCancellationSource::new(parent.token());
        let token = child.token().with_timeout(Duration::from_millis(1));

        std::thread::sleep(Duration::from_millis(10));

        assert!(token.is_stopped());
    }

    #[test]
    fn child_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ChildCancellationSource>();
        assert_send_sync::<ChildCancellationToken>();
    }
}

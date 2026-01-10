//! Cancellation source - owns the cancellation state.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::CancellationToken;

/// Owns the cancellation state and creates tokens.
///
/// This is the "source" side of cancellation. Create one of these, then
/// call [`token()`](Self::token) to get tokens that can be passed to
/// library functions.
///
/// The source must outlive all tokens created from it.
///
/// # Example
///
/// ```rust
/// use enough_std::CancellationSource;
/// use enough::Stop;
///
/// let source = CancellationSource::new();
/// let token = source.token();
///
/// // Not cancelled yet
/// assert!(!token.is_stopped());
///
/// // Cancel
/// source.cancel();
///
/// // Now stopped
/// assert!(token.is_stopped());
/// ```
pub struct CancellationSource {
    cancelled: AtomicBool,
}

impl CancellationSource {
    /// Create a new cancellation source.
    #[inline]
    pub fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
        }
    }

    /// Signal cancellation.
    ///
    /// All tokens created from this source will immediately start
    /// returning `Err(StopReason::Cancelled)` from `check()`.
    ///
    /// This is idempotent - calling it multiple times has no additional effect.
    #[inline]
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Check if cancellation has been requested.
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Reset the cancellation state.
    ///
    /// This allows the source to be reused. Use with caution - tokens
    /// that were already checked will not re-check automatically.
    #[inline]
    pub fn reset(&self) {
        self.cancelled.store(false, Ordering::Release);
    }

    /// Get a token that can be passed to library functions.
    ///
    /// The token is `Copy` and lightweight - just a pointer.
    /// It remains valid as long as this source exists.
    #[inline]
    pub fn token(&self) -> CancellationToken {
        CancellationToken::from_source(self)
    }

    /// Cancel after a duration, spawning a thread.
    ///
    /// This is a convenience method for simple timeout scenarios.
    /// For more control, use a dedicated timer or async runtime.
    ///
    /// # Example
    ///
    /// ```rust
    /// use enough_std::CancellationSource;
    /// use std::time::Duration;
    /// use std::sync::Arc;
    ///
    /// let source = Arc::new(CancellationSource::new());
    /// source.cancel_after(Duration::from_secs(5));
    ///
    /// // Source will be cancelled in 5 seconds
    /// ```
    pub fn cancel_after(self: &std::sync::Arc<Self>, duration: Duration) {
        let source = std::sync::Arc::clone(self);
        thread::spawn(move || {
            thread::sleep(duration);
            source.cancel();
        });
    }

    /// Get a raw pointer to the internal flag.
    ///
    /// This is useful for FFI or creating custom token implementations.
    ///
    /// # Safety
    ///
    /// The returned pointer is valid as long as the source exists.
    /// Do not dereference after the source is dropped.
    #[inline]
    pub fn flag_ptr(&self) -> *const AtomicBool {
        &self.cancelled
    }
}

impl Default for CancellationSource {
    fn default() -> Self {
        Self::new()
    }
}

// Debug impl that doesn't expose internal details
impl std::fmt::Debug for CancellationSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancellationSource")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use enough::Stop;

    #[test]
    fn source_starts_not_cancelled() {
        let source = CancellationSource::new();
        assert!(!source.is_cancelled());
    }

    #[test]
    fn source_cancel_works() {
        let source = CancellationSource::new();
        source.cancel();
        assert!(source.is_cancelled());
    }

    #[test]
    fn source_cancel_is_idempotent() {
        let source = CancellationSource::new();
        source.cancel();
        source.cancel();
        source.cancel();
        assert!(source.is_cancelled());
    }

    #[test]
    fn source_reset_works() {
        let source = CancellationSource::new();
        source.cancel();
        assert!(source.is_cancelled());
        source.reset();
        assert!(!source.is_cancelled());
    }

    #[test]
    fn token_reflects_source() {
        let source = CancellationSource::new();
        let token = source.token();

        assert!(!token.is_stopped());
        source.cancel();
        assert!(token.is_stopped());
    }

    #[test]
    fn multiple_tokens_share_state() {
        let source = CancellationSource::new();
        let token1 = source.token();
        let token2 = source.token();

        assert!(!token1.is_stopped());
        assert!(!token2.is_stopped());

        source.cancel();

        assert!(token1.is_stopped());
        assert!(token2.is_stopped());
    }

    #[test]
    fn source_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CancellationSource>();
    }

    #[test]
    fn default_impl() {
        let source: CancellationSource = Default::default();
        assert!(!source.is_cancelled());
    }

    #[test]
    fn debug_impl() {
        let source = CancellationSource::new();
        let debug = format!("{:?}", source);
        assert!(debug.contains("CancellationSource"));
        assert!(debug.contains("cancelled"));
    }
}

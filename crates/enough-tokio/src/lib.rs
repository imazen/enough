//! # enough-tokio
//!
//! Bridge tokio's `CancellationToken` to the [`Stop`] trait.
//!
//! ## When to Use
//!
//! Use this crate when you have:
//! - Tokio async code that needs to cancel CPU-intensive sync work in `spawn_blocking`
//! - Libraries that accept `impl Stop` and you want to use tokio's cancellation
//!
//! ## Complete Example
//!
//! ```rust,no_run
//! use enough_tokio::TokioStop;
//! use enough::Stop;
//! use tokio_util::sync::CancellationToken;
//!
//! #[tokio::main]
//! async fn main() {
//!     let token = CancellationToken::new();
//!     let stop = TokioStop::new(token.clone());
//!
//!     // Spawn CPU-intensive work
//!     let handle = tokio::task::spawn_blocking(move || {
//!         for i in 0..1_000_000 {
//!             if i % 1000 == 0 && stop.should_stop() {
//!                 return Err("cancelled");
//!             }
//!             // ... do work ...
//!         }
//!         Ok("done")
//!     });
//!
//!     // Cancel after timeout
//!     tokio::time::sleep(std::time::Duration::from_millis(10)).await;
//!     token.cancel();
//!
//!     let result = handle.await.unwrap();
//!     println!("{:?}", result);
//! }
//! ```
//!
//! ## Quick Reference
//!
//! ```rust,no_run
//! # use enough_tokio::TokioStop;
//! # use enough::Stop;
//! # use tokio_util::sync::CancellationToken;
//! let token = CancellationToken::new();
//! let stop = TokioStop::new(token.clone());
//!
//! stop.should_stop();         // Check if cancelled (sync)
//! stop.cancel();              // Trigger cancellation
//! // stop.cancelled().await;  // Wait for cancellation (async)
//! let child = stop.child();   // Create child token
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]

use enough::{Stop, StopReason};
use tokio_util::sync::CancellationToken;

/// Wrapper around tokio's [`CancellationToken`] that implements [`Stop`].
///
/// This allows using tokio's cancellation system with libraries that
/// accept `impl Stop`.
///
/// # Example
///
/// ```rust
/// use enough_tokio::TokioStop;
/// use enough::Stop;
/// use tokio_util::sync::CancellationToken;
///
/// let token = CancellationToken::new();
/// let stop = TokioStop::new(token.clone());
///
/// assert!(!stop.should_stop());
///
/// token.cancel();
///
/// assert!(stop.should_stop());
/// ```
#[derive(Clone)]
pub struct TokioStop {
    token: CancellationToken,
}

impl TokioStop {
    /// Create a new TokioStop from a CancellationToken.
    #[inline]
    pub fn new(token: CancellationToken) -> Self {
        Self { token }
    }

    /// Get the underlying CancellationToken.
    #[inline]
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// Get a clone of the underlying CancellationToken.
    #[inline]
    pub fn into_token(self) -> CancellationToken {
        self.token
    }

    /// Wait for cancellation.
    ///
    /// This is an async method for use in async contexts.
    #[inline]
    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }

    /// Create a child token that is cancelled when this one is.
    #[inline]
    pub fn child(&self) -> TokioStop {
        Self::new(self.token.child_token())
    }

    /// Cancel the token.
    #[inline]
    pub fn cancel(&self) {
        self.token.cancel();
    }
}

impl Stop for TokioStop {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        if self.token.is_cancelled() {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }

    #[inline]
    fn should_stop(&self) -> bool {
        self.token.is_cancelled()
    }
}

impl From<CancellationToken> for TokioStop {
    fn from(token: CancellationToken) -> Self {
        Self::new(token)
    }
}

impl From<TokioStop> for CancellationToken {
    fn from(stop: TokioStop) -> Self {
        stop.token
    }
}

impl std::fmt::Debug for TokioStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokioStop")
            .field("cancelled", &self.token.is_cancelled())
            .finish()
    }
}

/// Extension trait for CancellationToken to easily convert to Stop.
///
/// Named `CancellationTokenStopExt` to avoid potential conflicts if
/// `tokio_util` ever adds a `CancellationTokenExt` trait.
pub trait CancellationTokenStopExt {
    /// Convert to a TokioStop for use with `impl Stop` APIs.
    fn as_stop(&self) -> TokioStop;
}

impl CancellationTokenStopExt for CancellationToken {
    fn as_stop(&self) -> TokioStop {
        TokioStop::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokio_stop_reflects_token() {
        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());

        assert!(!stop.should_stop());
        assert!(stop.check().is_ok());

        token.cancel();

        assert!(stop.should_stop());
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn tokio_stop_child() {
        let parent = TokioStop::new(CancellationToken::new());
        let child = parent.child();

        assert!(!child.should_stop());

        parent.cancel();

        assert!(child.should_stop());
    }

    #[test]
    fn tokio_stop_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TokioStop>();
    }

    #[test]
    fn tokio_stop_clone() {
        let token = CancellationToken::new();
        let stop1 = TokioStop::new(token.clone());
        let stop2 = stop1.clone();

        token.cancel();

        assert!(stop1.should_stop());
        assert!(stop2.should_stop());
    }

    #[test]
    fn from_conversions() {
        let token = CancellationToken::new();
        let stop: TokioStop = token.clone().into();
        let _token2: CancellationToken = stop.into();
    }

    #[test]
    fn extension_trait() {
        let token = CancellationToken::new();
        let stop = token.as_stop();

        assert!(!stop.should_stop());
        token.cancel();
        assert!(stop.should_stop());
    }

    #[tokio::test]
    async fn cancelled_async() {
        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());

        // Spawn a task that cancels after a delay
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            token.cancel();
        });

        // Wait for cancellation
        stop.cancelled().await;

        assert!(stop.should_stop());
    }

    #[tokio::test]
    async fn spawn_blocking_integration() {
        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());

        let handle = tokio::task::spawn_blocking(move || {
            let mut count = 0;
            for i in 0..1_000_000 {
                if i % 1000 == 0 && stop.should_stop() {
                    return Err("cancelled");
                }
                count += 1;
                // Simulate work
                std::hint::black_box(count);
            }
            Ok(count)
        });

        // Cancel quickly
        tokio::time::sleep(std::time::Duration::from_micros(100)).await;
        token.cancel();

        let result = handle.await.unwrap();
        // Either completed or cancelled - both are valid
        assert!(result.is_ok() || result == Err("cancelled"));
    }

    #[tokio::test]
    async fn select_with_cancellation() {
        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());

        // Spawn cancellation
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            token.cancel();
        });

        let result = tokio::select! {
            _ = stop.cancelled() => "cancelled",
            _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => "timeout",
        };

        assert_eq!(result, "cancelled");
    }

    #[tokio::test]
    async fn multiple_tasks_same_token() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let token = CancellationToken::new();
        let cancelled_count = Arc::new(AtomicUsize::new(0));

        let mut handles = vec![];

        for _ in 0..10 {
            let stop = TokioStop::new(token.clone());
            let cancelled_count = Arc::clone(&cancelled_count);

            handles.push(tokio::spawn(async move {
                for _ in 0..100 {
                    if stop.should_stop() {
                        cancelled_count.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            }));
        }

        // Cancel after some tasks have started
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        token.cancel();

        for h in handles {
            h.await.unwrap();
        }

        // At least some tasks should have been cancelled
        assert!(cancelled_count.load(Ordering::Relaxed) > 0);
    }

    #[tokio::test]
    async fn child_token_cancellation() {
        let parent = TokioStop::new(CancellationToken::new());
        let child1 = parent.child();
        let child2 = parent.child();

        assert!(!child1.should_stop());
        assert!(!child2.should_stop());

        // Cancel one child doesn't affect others
        child1.cancel();
        assert!(child1.should_stop());
        assert!(!child2.should_stop());
        assert!(!parent.should_stop());

        // Cancel parent affects remaining children
        parent.cancel();
        assert!(child2.should_stop());
    }

    #[tokio::test]
    async fn nested_child_tokens() {
        let root = TokioStop::new(CancellationToken::new());
        let level1 = root.child();
        let level2 = level1.child();
        let level3 = level2.child();

        assert!(!level3.should_stop());

        root.cancel();

        assert!(level1.should_stop());
        assert!(level2.should_stop());
        assert!(level3.should_stop());
    }

    #[tokio::test]
    async fn check_returns_correct_reason() {
        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());

        assert_eq!(stop.check(), Ok(()));

        token.cancel();

        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[tokio::test]
    async fn debug_formatting() {
        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());

        let debug = format!("{:?}", stop);
        assert!(debug.contains("TokioStop"));
        assert!(debug.contains("cancelled"));
        assert!(debug.contains("false"));

        token.cancel();

        let debug = format!("{:?}", stop);
        assert!(debug.contains("true"));
    }

    #[tokio::test]
    async fn integration_with_stop_trait() {
        fn process_sync(data: &[u8], stop: impl Stop) -> Result<usize, &'static str> {
            for (i, _chunk) in data.chunks(100).enumerate() {
                if i % 10 == 0 && stop.should_stop() {
                    return Err("cancelled");
                }
            }
            Ok(data.len())
        }

        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());
        let data = vec![0u8; 10000];

        // Not cancelled - completes
        let result = process_sync(&data, stop.clone());
        assert_eq!(result, Ok(10000));

        // Cancel and retry
        token.cancel();
        let result = process_sync(&data, stop);
        assert_eq!(result, Err("cancelled"));
    }

    #[tokio::test]
    async fn token_accessor_methods() {
        let original_token = CancellationToken::new();
        let stop = TokioStop::new(original_token.clone());

        // token() returns reference
        let token_ref = stop.token();
        assert!(!token_ref.is_cancelled());

        // into_token() consumes and returns owned token
        let recovered_token = stop.into_token();
        assert!(!recovered_token.is_cancelled());

        // Original token still works
        original_token.cancel();
        assert!(recovered_token.is_cancelled());
    }

    #[test]
    fn sync_send_bounds() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}

        assert_send::<TokioStop>();
        assert_sync::<TokioStop>();
    }

    #[tokio::test]
    async fn rapid_cancel_check_cycle() {
        // Stress test rapid cancellation
        for _ in 0..100 {
            let token = CancellationToken::new();
            let stop = TokioStop::new(token.clone());

            assert!(!stop.should_stop());
            token.cancel();
            assert!(stop.should_stop());
        }
    }

    #[tokio::test]
    async fn select_loop_with_pinned_cancelled() {
        use tokio::sync::mpsc;

        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());
        let (tx, mut rx) = mpsc::channel::<i32>(10);

        // Send some messages
        tx.send(1).await.unwrap();
        tx.send(2).await.unwrap();
        tx.send(3).await.unwrap();

        // Spawn cancellation after messages
        let token_clone = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            token_clone.cancel();
        });

        // Correct pattern: pin the future outside the loop
        let cancelled = stop.cancelled();
        tokio::pin!(cancelled);

        let mut received = vec![];
        let mut was_cancelled = false;

        loop {
            tokio::select! {
                _ = &mut cancelled => {
                    was_cancelled = true;
                    break;
                }
                msg = rx.recv() => {
                    match msg {
                        Some(m) => received.push(m),
                        None => break,
                    }
                }
            }
        }

        assert_eq!(received, vec![1, 2, 3]);
        assert!(was_cancelled);
    }

    #[tokio::test]
    async fn select_biased_cancellation_priority() {
        use tokio::sync::mpsc;

        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());
        let (tx, mut rx) = mpsc::channel::<i32>(10);

        // Pre-cancel before loop
        token.cancel();

        // Send a message (channel should still have it)
        tx.send(42).await.unwrap();

        let cancelled = stop.cancelled();
        tokio::pin!(cancelled);

        // With biased, cancellation should win since it's first
        let result = tokio::select! {
            biased;
            _ = &mut cancelled => "cancelled",
            _ = rx.recv() => "received",
        };

        assert_eq!(result, "cancelled");
    }
}

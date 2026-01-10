//! # enough-tokio
//!
//! Tokio integration for the [`enough`] cancellation trait.
//!
//! This crate provides adapters between tokio's cancellation primitives
//! and the [`Stop`] trait.
//!
//! ## Wrapping Tokio's CancellationToken
//!
//! ```rust,ignore
//! use enough_tokio::TokioStop;
//! use enough::Stop;
//! use tokio_util::sync::CancellationToken;
//!
//! let token = CancellationToken::new();
//! let stop = TokioStop::new(token.clone());
//!
//! // Use in blocking context
//! tokio::task::spawn_blocking(move || {
//!     for i in 0..1000 {
//!         stop.check()?;
//!         // do work...
//!     }
//!     Ok(())
//! });
//!
//! // Cancel from async context
//! token.cancel();
//! ```
//!
//! ## Async Waiting
//!
//! ```rust,ignore
//! use enough_tokio::TokioStop;
//!
//! let stop = TokioStop::new(token);
//!
//! // Wait for cancellation in async code
//! stop.cancelled().await;
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
pub trait CancellationTokenExt {
    /// Convert to a TokioStop for use with `impl Stop` APIs.
    fn as_stop(&self) -> TokioStop;
}

impl CancellationTokenExt for CancellationToken {
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
}

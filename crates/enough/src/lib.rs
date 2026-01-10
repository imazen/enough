//! # enough
//!
//! Minimal cooperative cancellation trait for long-running operations.
//!
//! This crate provides a shared [`Stop`] trait that codec authors and library
//! writers can use to support cancellation. It is `no_std` by default with
//! zero dependencies.
//!
//! ## For Library Authors
//!
//! Accept `impl Stop` in your long-running functions:
//!
//! ```rust
//! use enough::{Stop, StopReason};
//!
//! pub fn decode(data: &[u8], stop: impl Stop) -> Result<Vec<u8>, DecodeError> {
//!     let mut output = Vec::new();
//!     for (i, chunk) in data.chunks(1024).enumerate() {
//!         // Check periodically in hot loops
//!         if i % 16 == 0 {
//!             stop.check()?;
//!         }
//!         // process chunk...
//!         output.extend_from_slice(chunk);
//!     }
//!     Ok(output)
//! }
//!
//! #[derive(Debug)]
//! pub enum DecodeError {
//!     Stopped(StopReason),
//!     InvalidData,
//! }
//!
//! impl From<StopReason> for DecodeError {
//!     fn from(r: StopReason) -> Self { DecodeError::Stopped(r) }
//! }
//! ```
//!
//! ## For Application Developers
//!
//! ### Zero-Allocation (no_std)
//!
//! Use [`AtomicStop`] when the source outlives all tokens:
//!
//! ```rust
//! use enough::{AtomicStop, Stop};
//!
//! let source = AtomicStop::new();
//! let token = source.token();
//!
//! assert!(!token.should_stop());
//!
//! source.cancel();
//! assert!(token.should_stop());
//! ```
//!
//! ### Owned Tokens (alloc)
//!
//! Enable the `alloc` feature for [`ArcStop`] with owned, cloneable tokens:
//!
//! ```rust
//! # #[cfg(feature = "alloc")]
//! # fn main() {
//! use enough::{ArcStop, Stop};
//!
//! let source = ArcStop::new();
//! let token = source.token(); // Owned, can outlive source
//!
//! assert!(!token.should_stop());
//!
//! source.cancel();
//! assert!(token.should_stop());
//! # }
//! # #[cfg(not(feature = "alloc"))]
//! # fn main() {}
//! ```
//!
//! ### Timeouts (std)
//!
//! Enable the `std` feature for timeout support:
//!
//! ```rust
//! # #[cfg(feature = "std")]
//! # fn main() {
//! use enough::{ArcStop, Stop, TimeoutExt};
//! use std::time::Duration;
//!
//! let source = ArcStop::new();
//! let token = source.token().with_timeout(Duration::from_secs(30));
//!
//! // Token will stop if cancelled OR if 30 seconds pass
//! assert!(!token.should_stop());
//! # }
//! # #[cfg(not(feature = "std"))]
//! # fn main() {}
//! ```
//!
//! ## Feature Flags
//!
//! - **None (default)** - Core trait, `Never`, `AtomicStop`, `SyncStop`, `FnStop`, `OrStop`
//! - **`alloc`** - Adds `ArcStop`, `ArcToken`, `BoxStop`, `ChildSource`, `ChildToken`,
//!   and blanket impls for `Box<T>`, `Arc<T>`
//! - **`std`** - Implies `alloc`. Adds timeouts (`TimeoutExt`, `WithTimeout`) and
//!   `std::error::Error` impl for `StopReason`
//!
//! ## Type Overview
//!
//! | Type | Feature | Allocation | Use Case |
//! |------|---------|------------|----------|
//! | [`Never`] | core | None | Zero-cost "never stop" |
//! | [`AtomicStop`] / [`AtomicToken`] | core | None | Stack-based, Relaxed ordering |
//! | [`SyncStop`] / [`SyncToken`] | core | None | Stack-based, Acquire/Release ordering |
//! | [`FnStop`] | core | None | Wrap a closure |
//! | [`OrStop`] | core | None | Combine multiple stops |
//! | [`ArcStop`] / [`ArcToken`] | alloc | Heap | Owned tokens, can outlive source |
//! | [`BoxStop`] | alloc | Heap | Dynamic dispatch, avoid monomorphization |
//! | [`ChildSource`](children::ChildSource) | alloc | Heap | Hierarchical cancellation |
//! | [`WithTimeout`] | std | None | Add deadline to any `Stop` |

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]
#![warn(clippy::all)]

#[cfg(feature = "alloc")]
extern crate alloc;

// Core modules (no_std, no alloc)
mod atomic;
mod func;
mod or;
mod reason;
mod sync;

// Alloc-dependent modules
#[cfg(feature = "alloc")]
mod arc;
#[cfg(feature = "alloc")]
mod boxed;
#[cfg(feature = "alloc")]
pub mod children;

// Std-dependent modules
#[cfg(feature = "std")]
pub mod time;

// Re-exports: Core
pub use atomic::{AtomicStop, AtomicToken};
pub use func::FnStop;
pub use or::OrStop;
pub use reason::StopReason;
pub use sync::{SyncStop, SyncToken};

// Re-exports: Alloc
#[cfg(feature = "alloc")]
pub use arc::{ArcStop, ArcToken};
#[cfg(feature = "alloc")]
pub use boxed::BoxStop;

// Re-exports: Std
#[cfg(feature = "std")]
pub use time::{TimeoutExt, WithTimeout};

/// Cooperative cancellation check.
///
/// Implement this trait for custom cancellation sources. The implementation
/// must be thread-safe (`Send + Sync`) to support parallel processing and
/// async runtimes.
///
/// # Example Implementation
///
/// ```rust
/// use enough::{Stop, StopReason};
/// use core::sync::atomic::{AtomicBool, Ordering};
///
/// pub struct MyToken<'a> {
///     cancelled: &'a AtomicBool,
/// }
///
/// impl Stop for MyToken<'_> {
///     fn check(&self) -> Result<(), StopReason> {
///         if self.cancelled.load(Ordering::Relaxed) {
///             Err(StopReason::Cancelled)
///         } else {
///             Ok(())
///         }
///     }
/// }
/// ```
pub trait Stop: Send + Sync {
    /// Check if the operation should stop.
    ///
    /// Returns `Ok(())` to continue, `Err(StopReason)` to stop.
    ///
    /// Call this periodically in long-running loops. The frequency depends
    /// on your workload - typically every 16-1000 iterations is reasonable.
    fn check(&self) -> Result<(), StopReason>;

    /// Returns `true` if the operation should stop.
    ///
    /// Convenience method for when you want to handle stopping yourself
    /// rather than using the `?` operator.
    #[inline]
    fn should_stop(&self) -> bool {
        self.check().is_err()
    }
}

/// A [`Stop`] implementation that never stops.
///
/// This is a zero-cost type for callers who don't need cancellation support.
/// All methods are inlined and optimized away.
///
/// # Example
///
/// ```rust
/// use enough::{Stop, Never};
///
/// fn process(data: &[u8], stop: impl Stop) -> Vec<u8> {
///     // ...
///     # vec![]
/// }
///
/// // Caller doesn't need cancellation
/// let data = [1u8, 2, 3];
/// let result = process(&data, Never);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Never;

impl Stop for Never {
    #[inline(always)]
    fn check(&self) -> Result<(), StopReason> {
        Ok(())
    }

    #[inline(always)]
    fn should_stop(&self) -> bool {
        false
    }
}

// Blanket impl: &T where T: Stop
impl<T: Stop + ?Sized> Stop for &T {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        (**self).check()
    }

    #[inline]
    fn should_stop(&self) -> bool {
        (**self).should_stop()
    }
}

// Blanket impl: &mut T where T: Stop
impl<T: Stop + ?Sized> Stop for &mut T {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        (**self).check()
    }

    #[inline]
    fn should_stop(&self) -> bool {
        (**self).should_stop()
    }
}

#[cfg(feature = "alloc")]
impl<T: Stop + ?Sized> Stop for alloc::boxed::Box<T> {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        (**self).check()
    }

    #[inline]
    fn should_stop(&self) -> bool {
        (**self).should_stop()
    }
}

#[cfg(feature = "alloc")]
impl<T: Stop + ?Sized> Stop for alloc::sync::Arc<T> {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        (**self).check()
    }

    #[inline]
    fn should_stop(&self) -> bool {
        (**self).should_stop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_does_not_stop() {
        assert!(!Never.should_stop());
        assert!(Never.check().is_ok());
    }

    #[test]
    fn never_is_copy() {
        let a = Never;
        let b = a; // Copy
        let _ = a; // Still valid
        let _ = b;
    }

    #[test]
    fn never_is_default() {
        let _: Never = Default::default();
    }

    #[test]
    fn reference_impl_works() {
        let never = Never;
        let reference: &dyn Stop = &never;
        assert!(!reference.should_stop());
    }

    #[test]
    fn stop_reason_from_impl() {
        // Test that From<StopReason> pattern works
        #[derive(Debug, PartialEq)]
        #[allow(dead_code)]
        enum TestError {
            Stopped(StopReason),
            Other,
        }

        impl From<StopReason> for TestError {
            fn from(r: StopReason) -> Self {
                TestError::Stopped(r)
            }
        }

        fn might_stop(stop: impl Stop) -> Result<(), TestError> {
            stop.check()?;
            Ok(())
        }

        assert!(might_stop(Never).is_ok());
    }

    #[test]
    fn dyn_stop_works() {
        fn process(stop: &dyn Stop) -> bool {
            stop.should_stop()
        }

        let never = Never;
        assert!(!process(&never));

        let source = AtomicStop::new();
        assert!(!process(&source));

        source.cancel();
        assert!(process(&source));
    }
}

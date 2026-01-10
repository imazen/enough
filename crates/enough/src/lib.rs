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
//! Enable the `std` feature for concrete implementations:
//!
//! ```rust
//! # #[cfg(feature = "std")]
//! # fn main() {
//! use enough::{CancellationSource, Stop};
//! use std::time::Duration;
//!
//! let source = CancellationSource::new();
//! let token = source.token().with_timeout(Duration::from_secs(30));
//!
//! // Check in operations
//! assert!(!token.is_stopped());
//!
//! // Cancel when needed
//! source.cancel();
//! assert!(token.is_stopped());
//! # }
//! # #[cfg(not(feature = "std"))]
//! # fn main() {}
//! ```
//!
//! ## Feature Flags
//!
//! - `std` - Enables `CancellationSource`, `CancellationToken`, timeouts, and
//!   child cancellation. Also enables `std::error::Error` impl for `StopReason`.
//! - `alloc` - Enables blanket impls for `Box<T>` and `Arc<T>`

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]
#![warn(clippy::all)]

#[cfg(feature = "alloc")]
extern crate alloc;

mod reason;

#[cfg(feature = "std")]
mod callback;
#[cfg(feature = "std")]
mod child;
#[cfg(feature = "std")]
mod source;

pub use reason::StopReason;

#[cfg(feature = "std")]
pub use callback::{CallbackCancellation, CallbackCancellationToken};
#[cfg(feature = "std")]
pub use child::{ChildCancellationSource, ChildCancellationToken};
#[cfg(feature = "std")]
pub use source::{CancellationSource, CancellationToken};

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
///         if self.cancelled.load(Ordering::Acquire) {
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
    fn is_stopped(&self) -> bool {
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
    fn is_stopped(&self) -> bool {
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
    fn is_stopped(&self) -> bool {
        (**self).is_stopped()
    }
}

// Blanket impl: &mut T where T: Stop
impl<T: Stop + ?Sized> Stop for &mut T {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        (**self).check()
    }

    #[inline]
    fn is_stopped(&self) -> bool {
        (**self).is_stopped()
    }
}

#[cfg(feature = "alloc")]
impl<T: Stop + ?Sized> Stop for alloc::boxed::Box<T> {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        (**self).check()
    }

    #[inline]
    fn is_stopped(&self) -> bool {
        (**self).is_stopped()
    }
}

#[cfg(feature = "alloc")]
impl<T: Stop + ?Sized> Stop for alloc::sync::Arc<T> {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        (**self).check()
    }

    #[inline]
    fn is_stopped(&self) -> bool {
        (**self).is_stopped()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_does_not_stop() {
        assert!(!Never.is_stopped());
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
        assert!(!reference.is_stopped());
    }

    #[test]
    fn stop_reason_from_impl() {
        // Test that From<StopReason> pattern works
        #[derive(Debug, PartialEq)]
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
}

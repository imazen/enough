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
//! ### The Default: Stopper
//!
//! [`Stopper`] is the recommended type for most use cases:
//!
//! ```rust
//! # #[cfg(feature = "alloc")]
//! # fn main() {
//! use enough::{Stopper, Stop};
//!
//! let stop = Stopper::new();
//! let stop2 = stop.clone();  // Clone to share
//!
//! // Pass to operations
//! assert!(!stop2.should_stop());
//!
//! // Any clone can cancel
//! stop.cancel();
//! assert!(stop2.should_stop());
//! # }
//! # #[cfg(not(feature = "alloc"))]
//! # fn main() {}
//! ```
//!
//! ### Zero-Allocation (no_std)
//!
//! Use [`StopSource`]/[`StopRef`] for stack-based cancellation:
//!
//! ```rust
//! use enough::{StopSource, Stop};
//!
//! let source = StopSource::new();
//! let stop = source.as_ref();  // Borrowed reference
//!
//! assert!(!stop.should_stop());
//!
//! source.cancel();
//! assert!(stop.should_stop());
//! ```
//!
//! ### Timeouts (std)
//!
//! Enable the `std` feature for timeout support:
//!
//! ```rust
//! # #[cfg(feature = "std")]
//! # fn main() {
//! use enough::{Stopper, Stop, TimeoutExt};
//! use std::time::Duration;
//!
//! let stop = Stopper::new();
//! let timed = stop.clone().with_timeout(Duration::from_secs(30));
//!
//! // Stops if cancelled OR if 30 seconds pass
//! assert!(!timed.should_stop());
//! # }
//! # #[cfg(not(feature = "std"))]
//! # fn main() {}
//! ```
//!
//! ## Feature Flags
//!
//! - **None (default)** - Core trait, `Never`, `StopSource`, `StopRef`, `FnStop`, `OrStop`
//! - **`alloc`** - Adds `Stopper`, `SyncStopper`, `TreeStopper`, `BoxedStop`,
//!   and blanket impls for `Box<T>`, `Arc<T>`
//! - **`std`** - Implies `alloc`. Adds timeouts (`TimeoutExt`, `WithTimeout`) and
//!   `std::error::Error` impl for `StopReason`
//!
//! ## Type Overview
//!
//! | Type | Feature | Use Case |
//! |------|---------|----------|
//! | [`Never`] | core | Zero-cost "never stop" |
//! | [`StopSource`] / [`StopRef`] | core | Stack-based, borrowed, zero-alloc |
//! | [`FnStop`] | core | Wrap any closure |
//! | [`OrStop`] | core | Combine multiple stops |
//! | [`Stopper`] | alloc | **Default choice** - Arc-based, clone to share |
//! | [`SyncStopper`] | alloc | Like Stopper with Acquire/Release ordering |
//! | [`TreeStopper`] | alloc | Hierarchical parent-child cancellation |
//! | [`BoxedStop`] | alloc | Type-erased dynamic dispatch |
//! | [`WithTimeout`] | std | Add deadline to any `Stop` |

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]
#![warn(clippy::all)]

#[cfg(feature = "alloc")]
extern crate alloc;

// Core modules (no_std, no alloc)
mod func;
mod or;
mod reason;
mod source;

// Alloc-dependent modules
#[cfg(feature = "alloc")]
mod boxed;
#[cfg(feature = "alloc")]
mod stopper;
#[cfg(feature = "alloc")]
mod sync_stopper;
#[cfg(feature = "alloc")]
mod tree;

// Std-dependent modules
#[cfg(feature = "std")]
pub mod time;

// Re-exports: Core
pub use func::FnStop;
pub use or::OrStop;
pub use reason::StopReason;
pub use source::{StopRef, StopSource};

// Re-exports: Alloc
#[cfg(feature = "alloc")]
pub use boxed::BoxedStop;
#[cfg(feature = "alloc")]
pub use stopper::Stopper;
#[cfg(feature = "alloc")]
pub use sync_stopper::SyncStopper;
#[cfg(feature = "alloc")]
pub use tree::TreeStopper;

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
/// pub struct MyStop<'a> {
///     cancelled: &'a AtomicBool,
/// }
///
/// impl Stop for MyStop<'_> {
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

        let source = StopSource::new();
        assert!(!process(&source));

        source.cancel();
        assert!(process(&source));
    }
}

//! # enough
//!
//! Minimal cooperative cancellation trait. Zero dependencies, `no_std` compatible.
//!
//! ## Which Crate?
//!
//! - **Library authors**: Use this crate (`enough`) - minimal, zero deps
//! - **Application code**: Use [`almost-enough`](https://docs.rs/almost-enough) for concrete types
//!
//! ## For Library Authors
//!
//! Accept `impl Stop` as the last parameter. Re-export `Unstoppable` for callers who don't need cancellation:
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
//! ## Zero-Cost When Not Needed
//!
//! Use [`Unstoppable`] when you don't need cancellation:
//!
//! ```rust
//! use enough::Unstoppable;
//!
//! // Compiles to nothing - zero runtime cost
//! // let result = my_codec::decode(&data, Unstoppable);
//! ```
//!
//! ## Implementations
//!
//! This crate provides only the trait and a zero-cost `Unstoppable` implementation.
//! For concrete cancellation primitives (`Stopper`, `StopSource`, timeouts, etc.),
//! see the [`almost-enough`](https://docs.rs/almost-enough) crate.
//!
//! ## Feature Flags
//!
//! - **None (default)** - Core trait only, `no_std` compatible
//! - **`std`** - Implies `alloc` (kept for downstream compatibility)

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]
#![warn(clippy::all)]

#[cfg(feature = "alloc")]
extern crate alloc;

mod reason;

pub use reason::StopReason;

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

    /// Returns `true` if this stop can ever signal a stop.
    ///
    /// [`Unstoppable`] returns `false`. Wrapper types delegate to their
    /// inner stop. The default is `true` (conservative — always check).
    ///
    /// Use this with `impl Stop for Option<T>` to skip checks in hot loops
    /// behind `dyn Stop`:
    ///
    /// ```rust
    /// use enough::{Stop, StopReason, Unstoppable};
    ///
    /// fn process(stop: &dyn Stop) -> Result<(), StopReason> {
    ///     let stop = stop.may_stop().then_some(stop);
    ///     // stop is Option<&dyn Stop>, which impl Stop:
    ///     // None → check() always returns Ok(()), Some → delegates
    ///     for i in 0..100 {
    ///         stop.check()?;
    ///     }
    ///     Ok(())
    /// }
    ///
    /// // Unstoppable: may_stop() returns false, so stop is None
    /// assert!(process(&Unstoppable).is_ok());
    /// ```
    ///
    /// In generic code (`impl Stop`), this is unnecessary — the compiler
    /// already optimizes `Unstoppable::check()` to nothing via inlining.
    /// Use `may_stop()` only when accepting `&dyn Stop`.
    #[inline]
    fn may_stop(&self) -> bool {
        true
    }
}

/// A [`Stop`] implementation that never stops (no cooperative cancellation).
///
/// This is a zero-cost type for callers who don't need cancellation support.
/// All methods are inlined and optimized away.
///
/// The name `Unstoppable` clearly communicates that this operation cannot be
/// cooperatively cancelled - there is no cancellation token to check.
///
/// # Example
///
/// ```rust
/// use enough::{Stop, Unstoppable};
///
/// fn process(data: &[u8], stop: impl Stop) -> Vec<u8> {
///     // ...
///     # vec![]
/// }
///
/// // Caller doesn't need cancellation
/// let data = [1u8, 2, 3];
/// let result = process(&data, Unstoppable);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Unstoppable;

/// Type alias for backwards compatibility.
///
/// New code should use [`Unstoppable`] instead, which more clearly
/// communicates that cooperative cancellation is not possible.
#[deprecated(since = "0.3.0", note = "Use `Unstoppable` instead for clarity")]
pub type Never = Unstoppable;

impl Stop for Unstoppable {
    #[inline(always)]
    fn check(&self) -> Result<(), StopReason> {
        Ok(())
    }

    #[inline(always)]
    fn should_stop(&self) -> bool {
        false
    }

    #[inline(always)]
    fn may_stop(&self) -> bool {
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

    #[inline]
    fn may_stop(&self) -> bool {
        (**self).may_stop()
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

    #[inline]
    fn may_stop(&self) -> bool {
        (**self).may_stop()
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

    #[inline]
    fn may_stop(&self) -> bool {
        (**self).may_stop()
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

    #[inline]
    fn may_stop(&self) -> bool {
        (**self).may_stop()
    }
}

/// `Option<T>` implements `Stop`: `None` is a no-op (always `Ok(())`),
/// `Some(inner)` delegates to the inner stop.
///
/// This enables the [`may_stop()`](Stop::may_stop) pattern for hot loops:
///
/// ```rust
/// use enough::{Stop, StopReason, Unstoppable};
///
/// fn hot_loop(stop: &dyn Stop) -> Result<(), StopReason> {
///     let stop = stop.may_stop().then_some(stop);
///     for i in 0..1000 {
///         stop.check()?; // None → Ok(()), Some → delegates
///     }
///     Ok(())
/// }
///
/// assert!(hot_loop(&Unstoppable).is_ok());
/// ```
impl<T: Stop> Stop for Option<T> {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        match self {
            Some(s) => s.check(),
            None => Ok(()),
        }
    }

    #[inline]
    fn should_stop(&self) -> bool {
        match self {
            Some(s) => s.should_stop(),
            None => false,
        }
    }

    #[inline]
    fn may_stop(&self) -> bool {
        match self {
            Some(s) => s.may_stop(),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unstoppable_does_not_stop() {
        assert!(!Unstoppable.should_stop());
        assert!(Unstoppable.check().is_ok());
    }

    #[test]
    fn unstoppable_is_copy() {
        let a = Unstoppable;
        let b = a; // Copy
        let _ = a; // Still valid
        let _ = b;
    }

    #[test]
    fn unstoppable_is_default() {
        let _: Unstoppable = Default::default();
    }

    #[test]
    fn reference_impl_works() {
        let unstoppable = Unstoppable;
        let reference: &dyn Stop = &unstoppable;
        assert!(!reference.should_stop());
    }

    #[test]
    #[allow(deprecated)]
    fn never_alias_works() {
        // Backwards compatibility
        let stop: Never = Unstoppable;
        assert!(!stop.should_stop());
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

        assert!(might_stop(Unstoppable).is_ok());
    }

    #[test]
    fn dyn_stop_works() {
        fn process(stop: &dyn Stop) -> bool {
            stop.should_stop()
        }

        let unstoppable = Unstoppable;
        assert!(!process(&unstoppable));
    }

    #[test]
    fn unstoppable_may_not_stop() {
        assert!(!Unstoppable.may_stop());
    }

    #[test]
    fn dyn_unstoppable_may_not_stop() {
        let stop: &dyn Stop = &Unstoppable;
        assert!(!stop.may_stop());
    }

    #[test]
    fn may_stop_via_reference() {
        let stop = &Unstoppable;
        assert!(!stop.may_stop());
    }

    #[test]
    fn option_none_is_noop() {
        let stop: Option<&dyn Stop> = None;
        assert!(stop.check().is_ok());
        assert!(!stop.should_stop());
        assert!(!stop.may_stop());
    }

    #[test]
    fn option_some_delegates() {
        use core::sync::atomic::{AtomicBool, Ordering};

        struct TestStop(AtomicBool);
        unsafe impl Send for TestStop {}
        unsafe impl Sync for TestStop {}
        impl Stop for TestStop {
            fn check(&self) -> Result<(), StopReason> {
                if self.0.load(Ordering::Relaxed) {
                    Err(StopReason::Cancelled)
                } else {
                    Ok(())
                }
            }
        }

        let inner = TestStop(AtomicBool::new(false));
        let stop: Option<&dyn Stop> = Some(&inner);
        assert!(stop.check().is_ok());
        assert!(!stop.should_stop());
        assert!(stop.may_stop());

        inner.0.store(true, Ordering::Relaxed);
        assert!(stop.should_stop());
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn may_stop_hot_loop_pattern() {
        fn process(stop: &dyn Stop) -> Result<(), StopReason> {
            let stop = stop.may_stop().then_some(stop);
            for _ in 0..100 {
                stop.check()?;
            }
            Ok(())
        }

        assert!(process(&Unstoppable).is_ok());
    }
}

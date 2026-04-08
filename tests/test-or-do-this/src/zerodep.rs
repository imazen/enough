//! Zero-dependency cooperative cancellation — owned, storable, clone-cheap.
//!
//! **Copy this single file into your crate** to support cooperative
//! cancellation without taking a dependency on `enough` (or anything
//! else). [`StopCheck`] is an owned, `'static`, clone-cheap handle
//! you can drop into any struct without propagating lifetime
//! parameters.
//!
//! Callers using `enough`, a raw `AtomicBool`, tokio's
//! `CancellationToken`, or any other source bridge in a single
//! closure. Only dependency: `alloc::sync::Arc` (so `no_std + alloc`
//! is fine; pure `no_std` is not).
//!
//! # Example
//!
//! ```rust
//! # use test_or_do_this::zerodep::{StopCheck, StopReason};
//! pub struct Decoder {
//!     stop: StopCheck,
//!     block_size: usize,
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
//!
//! impl Decoder {
//!     pub fn new(stop: StopCheck) -> Self {
//!         Self { stop, block_size: 1024 }
//!     }
//!
//!     pub fn decode(&self, data: &[u8]) -> Result<Vec<u8>, DecodeError> {
//!         let mut out = Vec::with_capacity(data.len());
//!         for (i, chunk) in data.chunks(self.block_size).enumerate() {
//!             if i % 16 == 0 { self.stop.check()?; }
//!             out.extend_from_slice(chunk);
//!         }
//!         Ok(out)
//!     }
//! }
//! ```
//!
//! # Call sites
//!
//! ```rust,ignore
//! // No cancellation — zero cost, no allocation.
//! Decoder::new(StopCheck::none())
//!
//! // Arc<AtomicBool> — one call, assumes Relaxed + Cancelled:
//! Decoder::new(StopCheck::from_atomic(flag.clone()))
//!
//! // enough — lossless bridge with may_stop() optimization:
//! Decoder::new(StopCheck::maybe(token.may_stop().then(|| {
//!     let t = token.clone();
//!     move || t.check().map_err(map_reason)
//! })))
//!
//! // tokio-util:
//! let t = cancel_token.clone();
//! Decoder::new(StopCheck::from_flag(move || t.is_cancelled()))
//! ```
//!
//! # Layout and cost
//!
//! [`StopCheck`] is 16 bytes (niche-optimized `Option<Arc<dyn>>` —
//! `None` is two nulls, `Some` is a fat pointer).
//!
//! - `StopCheck::none()` — zero allocations, `check()` is one
//!   perfectly-predicted branch that returns `Ok(())`.
//! - `StopCheck::new(f)` / `from_flag(f)` / `from_atomic(flag)` —
//!   one `Arc` allocation. Never reallocates.
//! - `check()` — one branch + one indirect call through the vtable
//!   when `Some`; one branch when `None`.
//! - [`Clone`] — atomic refcount increment (free for `None`).
//!
//! Same shape as `enough::StopToken`, backed by a closure instead of
//! a trait object so any cancellation source becomes a one-line bridge.
//!
//! # `no_std`
//!
//! Requires `alloc` (for `Arc`). Add `extern crate alloc;` to your
//! crate root if you're `no_std`. [`StopReason`] implements
//! [`core::error::Error`] unconditionally — no `std` feature needed.

#![allow(dead_code)]

extern crate alloc;
use alloc::sync::Arc;
use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};

/// Owned cooperative-cancellation check.
///
/// Clone-cheap (refcount), storable (`'static`, no lifetimes), and
/// bridgeable from any cancellation source via a closure. See the
/// [module docs](self) for the full pattern.
#[derive(Clone, Default)]
pub struct StopCheck {
    inner: Option<Arc<dyn Fn() -> Result<(), StopReason> + Send + Sync>>,
}

impl StopCheck {
    /// A `StopCheck` that never fires. No allocation, no runtime cost.
    ///
    /// This is the default value and is `const`, so it can be used
    /// in `const` contexts:
    ///
    /// ```rust
    /// # use test_or_do_this::zerodep::StopCheck;
    /// const NONE: StopCheck = StopCheck::none();
    /// assert!(NONE.check().is_ok());
    /// ```
    #[inline]
    pub const fn none() -> Self {
        Self { inner: None }
    }

    /// Wrap a reason-aware closure.
    ///
    /// The closure returns `Ok(())` to keep going, or
    /// `Err(reason)` to stop. This matches the return type of
    /// `enough::Stop::check()` exactly, so lossless bridging from
    /// `enough` sources is a single `move || token.check().map_err(...)`.
    /// Use [`from_flag`](Self::from_flag) or [`from_atomic`](Self::from_atomic)
    /// for the common bool-flag case.
    ///
    /// One `Arc` allocation, then clone-cheap forever.
    ///
    /// ```rust
    /// # use test_or_do_this::zerodep::{StopCheck, StopReason};
    /// let stop = StopCheck::new(|| Err(StopReason::TimedOut));
    /// assert_eq!(stop.check(), Err(StopReason::TimedOut));
    /// ```
    #[inline]
    pub fn new<F>(f: F) -> Self
    where
        F: Fn() -> Result<(), StopReason> + Send + Sync + 'static,
    {
        Self {
            inner: Some(Arc::new(f)),
        }
    }

    /// Wrap a bool-returning closure; fires with [`StopReason::Cancelled`]
    /// when it returns `true`.
    ///
    /// ```rust
    /// # use test_or_do_this::zerodep::{StopCheck, StopReason};
    /// let stop = StopCheck::from_flag(|| true);
    /// assert_eq!(stop.check(), Err(StopReason::Cancelled));
    /// ```
    #[inline]
    pub fn from_flag<F>(f: F) -> Self
    where
        F: Fn() -> bool + Send + Sync + 'static,
    {
        Self::new(move || {
            if f() {
                Err(StopReason::Cancelled)
            } else {
                Ok(())
            }
        })
    }

    /// Wrap an `Arc<AtomicBool>` — fires with [`StopReason::Cancelled`]
    /// when the flag is `true`. Uses `Relaxed` ordering.
    ///
    /// This is the most common "I just want a cancel button" pattern.
    /// For `Acquire` ordering or a different reason, use
    /// [`from_flag`](Self::from_flag) with your own closure.
    ///
    /// ```rust
    /// # use test_or_do_this::zerodep::{StopCheck, StopReason};
    /// use core::sync::atomic::AtomicBool;
    /// use std::sync::Arc;
    ///
    /// let flag = Arc::new(AtomicBool::new(false));
    /// let stop = StopCheck::from_atomic(flag.clone());
    ///
    /// assert!(stop.check().is_ok());
    /// flag.store(true, core::sync::atomic::Ordering::Relaxed);
    /// assert_eq!(stop.check(), Err(StopReason::Cancelled));
    /// ```
    #[inline]
    pub fn from_atomic(flag: Arc<AtomicBool>) -> Self {
        Self::from_flag(move || flag.load(Ordering::Relaxed))
    }

    /// Like [`new`](Self::new), but `None` collapses to
    /// [`StopCheck::none()`] — skipping the `Arc` allocation entirely.
    ///
    /// Pairs naturally with `bool::then` to bridge from sources that
    /// have a `may_stop()` method:
    ///
    /// ```rust,ignore
    /// StopCheck::maybe(token.may_stop().then(|| {
    ///     let t = token.clone();
    ///     move || t.check().map_err(map_reason)
    /// }))
    /// ```
    #[inline]
    pub fn maybe<F>(f: Option<F>) -> Self
    where
        F: Fn() -> Result<(), StopReason> + Send + Sync + 'static,
    {
        match f {
            Some(f) => Self::new(f),
            None => Self::none(),
        }
    }

    /// Like [`from_flag`](Self::from_flag), but `None` collapses to
    /// [`StopCheck::none()`].
    ///
    /// ```rust,ignore
    /// StopCheck::maybe_flag(stop.may_stop().then(|| {
    ///     let s = stop.clone();
    ///     move || s.check().is_err()
    /// }))
    /// ```
    #[inline]
    pub fn maybe_flag<F>(f: Option<F>) -> Self
    where
        F: Fn() -> bool + Send + Sync + 'static,
    {
        match f {
            Some(f) => Self::from_flag(f),
            None => Self::none(),
        }
    }

    /// Returns `Err(reason)` if the check fires, `Ok(())` otherwise.
    ///
    /// Use with `?` in hot loops; implement `From<StopReason>` for
    /// your error type so this plumbs through naturally.
    #[inline]
    pub fn check(&self) -> Result<(), StopReason> {
        match &self.inner {
            Some(f) => f(),
            None => Ok(()),
        }
    }

    /// Returns `true` if this check could ever fire.
    ///
    /// [`StopCheck::none()`] returns `false` — use this to skip
    /// cancellation checks entirely in very hot loops:
    ///
    /// ```rust
    /// # use test_or_do_this::zerodep::StopCheck;
    /// fn hot_loop(stop: &StopCheck) {
    ///     if stop.may_stop() {
    ///         // slow path — check periodically
    ///     } else {
    ///         // fast path — skip checks entirely
    ///     }
    /// }
    /// ```
    #[inline]
    pub fn may_stop(&self) -> bool {
        self.inner.is_some()
    }
}

impl fmt::Debug for StopCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StopCheck")
            .field("may_stop", &self.may_stop())
            .finish()
    }
}

/// Why an operation was stopped.
///
/// Matches the variants and semantics of `enough::StopReason` so
/// error-type impls are trivially portable between the two.
///
/// Implement `From<StopReason>` for your error type to use `?`
/// naturally with [`StopCheck::check`]:
///
/// ```rust
/// # use test_or_do_this::zerodep::StopReason;
/// #[derive(Debug)]
/// enum MyError {
///     Stopped(StopReason),
///     Io,
/// }
///
/// impl From<StopReason> for MyError {
///     fn from(r: StopReason) -> Self { MyError::Stopped(r) }
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StopReason {
    /// Operation was explicitly cancelled.
    Cancelled,
    /// Operation exceeded its deadline.
    TimedOut,
}

// Intentionally no `is_cancelled` / `is_timed_out` / `is_transient`
// helpers. `StopReason` is `#[non_exhaustive]`; match on it directly
// so future variants produce a compiler error at the match site
// instead of silently returning `false` from an `is_*` helper.

impl fmt::Display for StopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("operation cancelled"),
            Self::TimedOut => f.write_str("operation timed out"),
        }
    }
}

// `core::error::Error` is stable since Rust 1.81 — no `std` feature
// gate needed. The default `source()` returning `None` is correct for
// a leaf error type.
impl core::error::Error for StopReason {}

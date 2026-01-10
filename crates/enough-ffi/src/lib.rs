//! # enough-ffi
//!
//! FFI helpers for exposing cancellation across language boundaries.
//!
//! This crate provides C-compatible functions and types for use with
//! C#/.NET, Python, and other languages that can call C APIs.
//!
//! ## C# Integration Example
//!
//! ```csharp
//! // P/Invoke declarations
//! [DllImport("mylib")]
//! static extern IntPtr enough_cancellation_create();
//!
//! [DllImport("mylib")]
//! static extern void enough_cancellation_cancel(IntPtr handle);
//!
//! [DllImport("mylib")]
//! static extern void enough_cancellation_destroy(IntPtr handle);
//!
//! // Usage with CancellationToken
//! public static byte[] Decode(byte[] data, CancellationToken ct)
//! {
//!     var handle = enough_cancellation_create();
//!     try
//!     {
//!         // Bridge .NET CancellationToken to Rust
//!         using var registration = ct.Register(() =>
//!             enough_cancellation_cancel(handle));
//!
//!         return NativeMethods.decode(data, handle);
//!     }
//!     finally
//!     {
//!         enough_cancellation_destroy(handle);
//!     }
//! }
//! ```
//!
//! ## Rust FFI Functions
//!
//! ```rust
//! use enough_ffi::{FfiCancellationSource, FfiCancellationToken};
//! use enough::Stop;
//!
//! #[no_mangle]
//! pub extern "C" fn decode(
//!     data: *const u8,
//!     len: usize,
//!     cancel: *const FfiCancellationSource,
//! ) -> i32 {
//!     let token = unsafe { FfiCancellationToken::from_ptr(cancel) };
//!
//!     // Use token with any library that accepts impl Stop
//!     // my_codec::decode(data, token);
//!
//!     0
//! }
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]

use std::sync::atomic::{AtomicBool, Ordering};

use enough::{Stop, StopReason};

/// FFI-safe cancellation source.
///
/// This is the type that should be created and destroyed across FFI.
/// It owns the cancellation state.
///
/// Create with [`enough_cancellation_create`], destroy with
/// [`enough_cancellation_destroy`].
#[repr(C)]
pub struct FfiCancellationSource {
    cancelled: AtomicBool,
}

impl FfiCancellationSource {
    /// Create a new source (internal use).
    fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
        }
    }

    /// Cancel this source.
    #[inline]
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Check if cancelled.
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Lightweight token for checking cancellation.
///
/// Created from a pointer to [`FfiCancellationSource`].
/// Does not own the source - the source must outlive the token.
#[derive(Clone, Copy)]
pub struct FfiCancellationToken {
    ptr: *const FfiCancellationSource,
}

// SAFETY: Only reads atomics through the pointer
unsafe impl Send for FfiCancellationToken {}
unsafe impl Sync for FfiCancellationToken {}

impl FfiCancellationToken {
    /// Create a token from a pointer.
    ///
    /// # Safety
    ///
    /// - If `ptr` is non-null, it must point to a valid `FfiCancellationSource`
    /// - The source must outlive all uses of this token
    #[inline]
    pub const unsafe fn from_ptr(ptr: *const FfiCancellationSource) -> Self {
        Self { ptr }
    }

    /// Create a never-cancelled token.
    #[inline]
    pub const fn never() -> Self {
        Self {
            ptr: std::ptr::null(),
        }
    }
}

impl Stop for FfiCancellationToken {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        if self.ptr.is_null() {
            return Ok(());
        }
        // SAFETY: Caller guarantees ptr is valid
        if unsafe { (*self.ptr).is_cancelled() } {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }

    #[inline]
    fn is_stopped(&self) -> bool {
        if self.ptr.is_null() {
            return false;
        }
        unsafe { (*self.ptr).is_cancelled() }
    }
}

impl std::fmt::Debug for FfiCancellationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfiCancellationToken")
            .field("ptr", &self.ptr)
            .field("is_null", &self.ptr.is_null())
            .finish()
    }
}

// ============================================================================
// C FFI Functions
// ============================================================================

/// Create a new cancellation source.
///
/// Returns a pointer to the source. Must be destroyed with
/// [`enough_cancellation_destroy`].
///
/// # Safety
///
/// The returned pointer must be passed to `enough_cancellation_destroy`
/// when no longer needed.
#[no_mangle]
pub extern "C" fn enough_cancellation_create() -> *mut FfiCancellationSource {
    Box::into_raw(Box::new(FfiCancellationSource::new()))
}

/// Cancel a cancellation source.
///
/// After this call, any tokens created from this source will return
/// `Err(StopReason::Cancelled)` from `check()`.
///
/// # Safety
///
/// `ptr` must be a valid pointer returned by [`enough_cancellation_create`],
/// or null (which is a no-op).
#[no_mangle]
pub unsafe extern "C" fn enough_cancellation_cancel(ptr: *const FfiCancellationSource) {
    if let Some(source) = ptr.as_ref() {
        source.cancel();
    }
}

/// Check if a cancellation source is cancelled.
///
/// # Safety
///
/// `ptr` must be a valid pointer returned by [`enough_cancellation_create`],
/// or null (which returns false).
#[no_mangle]
pub unsafe extern "C" fn enough_cancellation_is_cancelled(
    ptr: *const FfiCancellationSource,
) -> bool {
    ptr.as_ref().map(|s| s.is_cancelled()).unwrap_or(false)
}

/// Destroy a cancellation source.
///
/// # Safety
///
/// - `ptr` must be a valid pointer returned by [`enough_cancellation_create`],
///   or null (which is a no-op)
/// - The pointer must not be used after this call
/// - All tokens created from this source must no longer be in use
#[no_mangle]
pub unsafe extern "C" fn enough_cancellation_destroy(ptr: *mut FfiCancellationSource) {
    if !ptr.is_null() {
        drop(Box::from_raw(ptr));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_source_create_cancel_destroy() {
        unsafe {
            let ptr = enough_cancellation_create();
            assert!(!ptr.is_null());

            assert!(!enough_cancellation_is_cancelled(ptr));

            enough_cancellation_cancel(ptr);

            assert!(enough_cancellation_is_cancelled(ptr));

            enough_cancellation_destroy(ptr);
        }
    }

    #[test]
    fn ffi_token_from_source() {
        unsafe {
            let ptr = enough_cancellation_create();
            let token = FfiCancellationToken::from_ptr(ptr);

            assert!(!token.is_stopped());
            assert!(token.check().is_ok());

            enough_cancellation_cancel(ptr);

            assert!(token.is_stopped());
            assert_eq!(token.check(), Err(StopReason::Cancelled));

            enough_cancellation_destroy(ptr);
        }
    }

    #[test]
    fn ffi_token_never() {
        let token = FfiCancellationToken::never();
        assert!(!token.is_stopped());
        assert!(token.check().is_ok());
    }

    #[test]
    fn ffi_null_safety() {
        unsafe {
            // These should be safe no-ops
            enough_cancellation_cancel(std::ptr::null());
            enough_cancellation_destroy(std::ptr::null_mut());
            assert!(!enough_cancellation_is_cancelled(std::ptr::null()));
        }
    }

    #[test]
    fn ffi_token_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FfiCancellationToken>();
    }

    #[test]
    fn ffi_with_enough_std() {
        // Test interop with enough-std types
        use enough_std::CancellationSource;

        let source = CancellationSource::new();
        let token = source.token();

        // Both implement Stop
        fn use_stop(stop: impl Stop) -> bool {
            stop.is_stopped()
        }

        assert!(!use_stop(token));
        assert!(!use_stop(FfiCancellationToken::never()));
    }
}

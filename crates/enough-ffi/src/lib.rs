//! # enough-ffi
//!
//! FFI helpers for exposing cancellation across language boundaries.
//!
//! This crate provides C-compatible functions and types for use with
//! C#/.NET, Python, Node.js, and other languages that can call C APIs.
//!
//! ## Safety Model
//!
//! This crate uses reference counting internally to prevent use-after-free:
//!
//! - Sources and tokens use `Arc` internally
//! - Destroying a source while tokens exist is safe - tokens remain valid
//!   but can never become cancelled (since no one can call cancel anymore)
//! - Each token must be explicitly destroyed when no longer needed
//!
//! ## C# Integration Example
//!
//! ```csharp
//! // P/Invoke declarations
//! [DllImport("mylib")]
//! static extern IntPtr enough_cancellation_create();
//!
//! [DllImport("mylib")]
//! static extern void enough_cancellation_cancel(IntPtr source);
//!
//! [DllImport("mylib")]
//! static extern void enough_cancellation_destroy(IntPtr source);
//!
//! [DllImport("mylib")]
//! static extern IntPtr enough_token_create(IntPtr source);
//!
//! [DllImport("mylib")]
//! static extern bool enough_token_is_cancelled(IntPtr token);
//!
//! [DllImport("mylib")]
//! static extern void enough_token_destroy(IntPtr token);
//!
//! // Usage with CancellationToken
//! public static byte[] Decode(byte[] data, CancellationToken ct)
//! {
//!     var source = enough_cancellation_create();
//!     var token = enough_token_create(source);
//!     try
//!     {
//!         using var registration = ct.Register(() =>
//!             enough_cancellation_cancel(source));
//!
//!         return NativeMethods.decode(data, token);
//!     }
//!     finally
//!     {
//!         enough_token_destroy(token);
//!         enough_cancellation_destroy(source);
//!     }
//! }
//! ```
//!
//! ## Rust FFI Functions
//!
//! ```rust
//! use enough_ffi::{enough_token_create, enough_token_destroy, FfiCancellationToken};
//! use enough::Stop;
//!
//! #[no_mangle]
//! pub extern "C" fn decode(
//!     data: *const u8,
//!     len: usize,
//!     token: *const FfiCancellationToken,
//! ) -> i32 {
//!     let stop = unsafe { FfiCancellationToken::from_ptr(token) };
//!
//!     // Use stop with any library that accepts impl Stop
//!     if stop.should_stop() {
//!         return -1; // Cancelled
//!     }
//!
//!     0
//! }
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use enough::{Stop, StopReason};

// ============================================================================
// Internal Types
// ============================================================================

/// Shared cancellation state, reference counted.
struct CancellationState {
    cancelled: AtomicBool,
}

impl CancellationState {
    fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
        }
    }

    #[inline]
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    #[inline]
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

// ============================================================================
// FFI Source
// ============================================================================

/// FFI-safe cancellation source.
///
/// This is the type that should be created and destroyed across FFI.
/// It owns a reference to the shared cancellation state.
///
/// Create with [`enough_cancellation_create`], destroy with
/// [`enough_cancellation_destroy`].
///
/// **Safety**: This type uses `Arc` internally. Destroying the source while
/// tokens exist is safe - tokens will continue to work but can never become
/// cancelled.
#[repr(C)]
pub struct FfiCancellationSource {
    inner: Arc<CancellationState>,
}

impl FfiCancellationSource {
    fn new() -> Self {
        Self {
            inner: Arc::new(CancellationState::new()),
        }
    }

    /// Cancel this source.
    #[inline]
    pub fn cancel(&self) {
        self.inner.cancel();
    }

    /// Check if cancelled.
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    /// Create a token from this source.
    fn create_token(&self) -> FfiCancellationToken {
        FfiCancellationToken {
            inner: Some(Arc::clone(&self.inner)),
        }
    }
}

// ============================================================================
// FFI Token
// ============================================================================

/// FFI-safe cancellation token.
///
/// This token holds a reference to the shared cancellation state.
/// It must be explicitly destroyed with [`enough_token_destroy`].
///
/// The token remains valid even after the source is destroyed - it will
/// just never become cancelled.
#[repr(C)]
pub struct FfiCancellationToken {
    inner: Option<Arc<CancellationState>>,
}

impl FfiCancellationToken {
    /// Create a "never cancelled" token.
    ///
    /// This token will never report as cancelled.
    #[inline]
    pub fn never() -> Self {
        Self { inner: None }
    }

    /// Create a token view from a raw pointer.
    ///
    /// This creates a non-owning view that can be used to check cancellation.
    /// The original token must remain valid for the lifetime of this view.
    ///
    /// # Safety
    ///
    /// - If `ptr` is non-null, it must point to a valid `FfiCancellationToken`
    /// - The pointed-to token must outlive all uses of the returned view
    #[inline]
    pub unsafe fn from_ptr(ptr: *const FfiCancellationToken) -> FfiCancellationTokenView {
        FfiCancellationTokenView { ptr }
    }
}

impl Stop for FfiCancellationToken {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        match &self.inner {
            Some(state) if state.is_cancelled() => Err(StopReason::Cancelled),
            _ => Ok(()),
        }
    }

    #[inline]
    fn should_stop(&self) -> bool {
        self.inner
            .as_ref()
            .map(|s| s.is_cancelled())
            .unwrap_or(false)
    }
}

impl std::fmt::Debug for FfiCancellationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfiCancellationToken")
            .field("is_cancelled", &self.should_stop())
            .field("is_never", &self.inner.is_none())
            .finish()
    }
}

// ============================================================================
// Token View (for Rust code receiving token pointers)
// ============================================================================

/// A non-owning view of a cancellation token.
///
/// This is used by Rust FFI functions that receive a token pointer.
/// It does not own the token and does not affect reference counts.
#[derive(Clone, Copy)]
pub struct FfiCancellationTokenView {
    ptr: *const FfiCancellationToken,
}

// SAFETY: The view only reads through the pointer, and the underlying
// Arc<CancellationState> is Send + Sync.
unsafe impl Send for FfiCancellationTokenView {}
unsafe impl Sync for FfiCancellationTokenView {}

impl FfiCancellationTokenView {
    /// Create a "never cancelled" view.
    #[inline]
    pub const fn never() -> Self {
        Self {
            ptr: std::ptr::null(),
        }
    }
}

impl Stop for FfiCancellationTokenView {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        if self.ptr.is_null() {
            return Ok(());
        }
        // SAFETY: Caller guarantees ptr is valid
        unsafe {
            if (*self.ptr).should_stop() {
                Err(StopReason::Cancelled)
            } else {
                Ok(())
            }
        }
    }

    #[inline]
    fn should_stop(&self) -> bool {
        if self.ptr.is_null() {
            return false;
        }
        // SAFETY: Caller guarantees ptr is valid
        unsafe { (*self.ptr).should_stop() }
    }
}

impl std::fmt::Debug for FfiCancellationTokenView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfiCancellationTokenView")
            .field("ptr", &self.ptr)
            .field("is_null", &self.ptr.is_null())
            .finish()
    }
}

// ============================================================================
// C FFI Functions - Source Management
// ============================================================================

/// Create a new cancellation source.
///
/// Returns a pointer to the source. Must be destroyed with
/// [`enough_cancellation_destroy`].
///
/// Returns null if allocation fails.
#[no_mangle]
pub extern "C" fn enough_cancellation_create() -> *mut FfiCancellationSource {
    Box::into_raw(Box::new(FfiCancellationSource::new()))
}

/// Cancel a cancellation source.
///
/// After this call, any tokens created from this source will report
/// as cancelled.
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
/// This is safe to call even if tokens created from this source still exist.
/// Those tokens will remain valid but will never become cancelled.
///
/// # Safety
///
/// - `ptr` must be a valid pointer returned by [`enough_cancellation_create`],
///   or null (which is a no-op)
/// - The pointer must not be used after this call
#[no_mangle]
pub unsafe extern "C" fn enough_cancellation_destroy(ptr: *mut FfiCancellationSource) {
    if !ptr.is_null() {
        drop(Box::from_raw(ptr));
    }
}

// ============================================================================
// C FFI Functions - Token Management
// ============================================================================

/// Create a token from a cancellation source.
///
/// The token holds a reference to the shared state and must be destroyed
/// with [`enough_token_destroy`].
///
/// The token remains valid even after the source is destroyed.
///
/// # Safety
///
/// `source` must be a valid pointer returned by [`enough_cancellation_create`],
/// or null (which creates a "never cancelled" token).
#[no_mangle]
pub unsafe extern "C" fn enough_token_create(
    source: *const FfiCancellationSource,
) -> *mut FfiCancellationToken {
    let token = match source.as_ref() {
        Some(s) => s.create_token(),
        None => FfiCancellationToken::never(),
    };
    Box::into_raw(Box::new(token))
}

/// Create a "never cancelled" token.
///
/// This token will never report as cancelled. Must be destroyed with
/// [`enough_token_destroy`].
#[no_mangle]
pub extern "C" fn enough_token_create_never() -> *mut FfiCancellationToken {
    Box::into_raw(Box::new(FfiCancellationToken::never()))
}

/// Check if a token is cancelled.
///
/// # Safety
///
/// `token` must be a valid pointer returned by [`enough_token_create`],
/// or null (which returns false).
#[no_mangle]
pub unsafe extern "C" fn enough_token_is_cancelled(token: *const FfiCancellationToken) -> bool {
    token.as_ref().map(|t| t.should_stop()).unwrap_or(false)
}

/// Destroy a token.
///
/// # Safety
///
/// - `token` must be a valid pointer returned by [`enough_token_create`],
///   or null (which is a no-op)
/// - The pointer must not be used after this call
#[no_mangle]
pub unsafe extern "C" fn enough_token_destroy(token: *mut FfiCancellationToken) {
    if !token.is_null() {
        drop(Box::from_raw(token));
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_create_cancel_destroy() {
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
    fn token_lifecycle() {
        unsafe {
            let source = enough_cancellation_create();
            let token = enough_token_create(source);

            assert!(!enough_token_is_cancelled(token));

            enough_cancellation_cancel(source);

            assert!(enough_token_is_cancelled(token));

            enough_token_destroy(token);
            enough_cancellation_destroy(source);
        }
    }

    #[test]
    fn token_survives_source_destruction() {
        unsafe {
            let source = enough_cancellation_create();

            // Cancel before creating token
            enough_cancellation_cancel(source);

            let token = enough_token_create(source);

            // Destroy source while token exists - this is now safe!
            enough_cancellation_destroy(source);

            // Token should still report cancelled
            assert!(enough_token_is_cancelled(token));

            enough_token_destroy(token);
        }
    }

    #[test]
    fn token_from_destroyed_source_never_cancels() {
        unsafe {
            let source = enough_cancellation_create();
            let token = enough_token_create(source);

            // Destroy source without cancelling
            enough_cancellation_destroy(source);

            // Token should remain valid but never become cancelled
            // (no one can call cancel anymore)
            assert!(!enough_token_is_cancelled(token));

            enough_token_destroy(token);
        }
    }

    #[test]
    fn token_never() {
        unsafe {
            let token = enough_token_create_never();
            assert!(!enough_token_is_cancelled(token));
            enough_token_destroy(token);
        }
    }

    #[test]
    fn null_safety() {
        unsafe {
            // All of these should be safe no-ops
            enough_cancellation_cancel(std::ptr::null());
            enough_cancellation_destroy(std::ptr::null_mut());
            assert!(!enough_cancellation_is_cancelled(std::ptr::null()));

            enough_token_destroy(std::ptr::null_mut());
            assert!(!enough_token_is_cancelled(std::ptr::null()));

            // Null source creates never-cancelled token
            let token = enough_token_create(std::ptr::null());
            assert!(!enough_token_is_cancelled(token));
            enough_token_destroy(token);
        }
    }

    #[test]
    fn token_view_from_ptr() {
        unsafe {
            let source = enough_cancellation_create();
            let token = enough_token_create(source);

            // Rust code would receive the token pointer and create a view
            let view = FfiCancellationToken::from_ptr(token);

            assert!(!view.should_stop());
            assert!(view.check().is_ok());

            enough_cancellation_cancel(source);

            assert!(view.should_stop());
            assert_eq!(view.check(), Err(StopReason::Cancelled));

            enough_token_destroy(token);
            enough_cancellation_destroy(source);
        }
    }

    #[test]
    fn token_view_never() {
        let view = FfiCancellationTokenView::never();
        assert!(!view.should_stop());
        assert!(view.check().is_ok());
    }

    #[test]
    fn types_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FfiCancellationToken>();
        assert_send_sync::<FfiCancellationTokenView>();
    }

    #[test]
    fn multiple_tokens_same_source() {
        unsafe {
            let source = enough_cancellation_create();
            let t1 = enough_token_create(source);
            let t2 = enough_token_create(source);
            let t3 = enough_token_create(source);

            assert!(!enough_token_is_cancelled(t1));
            assert!(!enough_token_is_cancelled(t2));
            assert!(!enough_token_is_cancelled(t3));

            enough_cancellation_cancel(source);

            assert!(enough_token_is_cancelled(t1));
            assert!(enough_token_is_cancelled(t2));
            assert!(enough_token_is_cancelled(t3));

            // Destroy in different order than creation
            enough_token_destroy(t2);
            enough_cancellation_destroy(source);
            enough_token_destroy(t1);
            enough_token_destroy(t3);
        }
    }

    #[test]
    fn interop_with_enough() {
        // Both implement Stop
        fn use_stop(stop: impl Stop) -> bool {
            stop.should_stop()
        }

        // Test FfiCancellationToken with Stop trait
        assert!(!use_stop(FfiCancellationToken::never()));
        assert!(!use_stop(FfiCancellationTokenView::never()));

        // Test with a real source
        unsafe {
            let source = enough_cancellation_create();
            let token = enough_token_create(source);
            let view = FfiCancellationToken::from_ptr(token);

            assert!(!use_stop(view));

            enough_cancellation_cancel(source);
            assert!(use_stop(view));

            enough_token_destroy(token);
            enough_cancellation_destroy(source);
        }
    }

    #[test]
    fn concurrent_access_stress() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::thread;

        unsafe {
            let source = enough_cancellation_create();
            let cancelled_count = Arc::new(AtomicUsize::new(0));
            let check_count = Arc::new(AtomicUsize::new(0));

            // Create tokens upfront and convert to addresses
            let tokens: Vec<_> = (0..10)
                .map(|_| enough_token_create(source) as usize)
                .collect();

            // Spawn multiple threads that check cancellation
            let handles: Vec<_> = tokens
                .into_iter()
                .map(|token_addr| {
                    let cancelled_count = Arc::clone(&cancelled_count);
                    let check_count = Arc::clone(&check_count);

                    thread::spawn(move || {
                        let token = token_addr as *mut FfiCancellationToken;
                        let view = FfiCancellationToken::from_ptr(token);
                        for _ in 0..10000 {
                            check_count.fetch_add(1, Ordering::Relaxed);
                            if view.should_stop() {
                                cancelled_count.fetch_add(1, Ordering::Relaxed);
                                break;
                            }
                            thread::yield_now();
                        }
                        enough_token_destroy(token);
                    })
                })
                .collect();

            // Cancel after threads have started
            thread::sleep(std::time::Duration::from_millis(1));
            enough_cancellation_cancel(source);

            for h in handles {
                h.join().unwrap();
            }

            // All threads should have detected cancellation
            assert!(cancelled_count.load(Ordering::Relaxed) > 0);
            assert!(check_count.load(Ordering::Relaxed) > 0);

            enough_cancellation_destroy(source);
        }
    }

    #[test]
    fn cross_thread_cancellation() {
        use std::thread;

        unsafe {
            let source = enough_cancellation_create();
            let token = enough_token_create(source);

            // Send token to another thread
            let token_addr = token as usize;
            let handle = thread::spawn(move || {
                let token = token_addr as *const FfiCancellationToken;
                let view = FfiCancellationToken::from_ptr(token);

                // Spin until cancelled
                let mut iterations = 0;
                while !view.should_stop() && iterations < 1_000_000 {
                    iterations += 1;
                    thread::yield_now();
                }

                view.should_stop()
            });

            // Cancel from main thread
            thread::sleep(std::time::Duration::from_millis(5));
            enough_cancellation_cancel(source);

            let was_cancelled = handle.join().unwrap();
            assert!(was_cancelled);

            enough_token_destroy(token);
            enough_cancellation_destroy(source);
        }
    }

    #[test]
    fn rapid_create_destroy() {
        // Stress test allocation/deallocation
        unsafe {
            for _ in 0..1000 {
                let source = enough_cancellation_create();
                let tokens: Vec<_> = (0..10).map(|_| enough_token_create(source)).collect();

                enough_cancellation_cancel(source);

                for token in tokens {
                    assert!(enough_token_is_cancelled(token));
                    enough_token_destroy(token);
                }

                enough_cancellation_destroy(source);
            }
        }
    }

    #[test]
    fn idempotent_cancel() {
        unsafe {
            let source = enough_cancellation_create();
            let token = enough_token_create(source);

            // Cancel multiple times should be safe
            enough_cancellation_cancel(source);
            enough_cancellation_cancel(source);
            enough_cancellation_cancel(source);

            assert!(enough_token_is_cancelled(token));

            enough_token_destroy(token);
            enough_cancellation_destroy(source);
        }
    }

    #[test]
    fn token_view_copy_semantics() {
        unsafe {
            let source = enough_cancellation_create();
            let token = enough_token_create(source);

            let view1 = FfiCancellationToken::from_ptr(token);
            let view2 = view1; // Copy
            let view3 = view1; // Copy again

            assert!(!view1.should_stop());
            assert!(!view2.should_stop());
            assert!(!view3.should_stop());

            enough_cancellation_cancel(source);

            assert!(view1.should_stop());
            assert!(view2.should_stop());
            assert!(view3.should_stop());

            enough_token_destroy(token);
            enough_cancellation_destroy(source);
        }
    }

    #[test]
    fn check_returns_correct_reason() {
        unsafe {
            let source = enough_cancellation_create();
            let token = enough_token_create(source);
            let view = FfiCancellationToken::from_ptr(token);

            assert_eq!(view.check(), Ok(()));

            enough_cancellation_cancel(source);

            assert_eq!(view.check(), Err(StopReason::Cancelled));

            enough_token_destroy(token);
            enough_cancellation_destroy(source);
        }
    }

    #[test]
    fn debug_formatting() {
        unsafe {
            let source = enough_cancellation_create();
            let token = enough_token_create(source);
            let view = FfiCancellationToken::from_ptr(token);

            let token_ref = &*token;
            let token_debug = format!("{:?}", token_ref);
            assert!(token_debug.contains("FfiCancellationToken"));
            assert!(token_debug.contains("is_cancelled"));

            let view_debug = format!("{:?}", view);
            assert!(view_debug.contains("FfiCancellationTokenView"));

            enough_token_destroy(token);
            enough_cancellation_destroy(source);
        }
    }

    #[test]
    fn simulated_ffi_pattern() {
        // Simulates how a C caller would use this API
        unsafe {
            // 1. C code creates source and token
            let source = enough_cancellation_create();
            let token = enough_token_create(source);

            // 2. C code passes token pointer to Rust FFI function
            fn rust_ffi_function(
                token_ptr: *const FfiCancellationToken,
            ) -> Result<i32, &'static str> {
                let stop = unsafe { FfiCancellationToken::from_ptr(token_ptr) };

                for i in 0..1000 {
                    if i % 100 == 0 {
                        stop.check().map_err(|_| "cancelled")?;
                    }
                }
                Ok(42)
            }

            // 3. First call succeeds
            let result = rust_ffi_function(token);
            assert_eq!(result, Ok(42));

            // 4. C code triggers cancellation (e.g., from callback)
            enough_cancellation_cancel(source);

            // 5. Next call detects cancellation
            let result = rust_ffi_function(token);
            assert_eq!(result, Err("cancelled"));

            // 6. C code cleans up
            enough_token_destroy(token);
            enough_cancellation_destroy(source);
        }
    }
}

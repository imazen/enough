//! Tests for FFI helpers.
#![allow(unused_imports, dead_code)]

use enough::Stop;
use enough_ffi::{
    enough_cancellation_cancel, enough_cancellation_create, enough_cancellation_destroy,
    enough_cancellation_is_cancelled, FfiCancellationSource, FfiCancellationToken,
};
use std::thread;

#[test]
fn ffi_lifecycle() {
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
fn ffi_token_from_pointer() {
    unsafe {
        let ptr = enough_cancellation_create();
        let token = FfiCancellationToken::from_ptr(ptr);

        assert!(!token.is_stopped());

        enough_cancellation_cancel(ptr);

        assert!(token.is_stopped());

        enough_cancellation_destroy(ptr);
    }
}

#[test]
fn ffi_null_safety() {
    unsafe {
        // All of these should be no-ops or return safe defaults
        enough_cancellation_cancel(std::ptr::null());
        enough_cancellation_destroy(std::ptr::null_mut());
        assert!(!enough_cancellation_is_cancelled(std::ptr::null()));

        let token = FfiCancellationToken::from_ptr(std::ptr::null());
        assert!(!token.is_stopped());
    }
}

#[test]
fn ffi_never_token() {
    let token = FfiCancellationToken::never();
    assert!(!token.is_stopped());
    assert!(token.check().is_ok());
}

#[test]
fn ffi_cross_thread() {
    unsafe {
        let ptr = enough_cancellation_create();

        // Send pointer to another thread
        let ptr_addr = ptr as usize;
        let handle = thread::spawn(move || {
            let ptr = ptr_addr as *const FfiCancellationSource;
            let token = FfiCancellationToken::from_ptr(ptr);

            // Wait for cancellation
            while !token.is_stopped() {
                thread::yield_now();
            }

            true
        });

        // Cancel from main thread
        thread::sleep(std::time::Duration::from_millis(10));
        enough_cancellation_cancel(ptr);

        assert!(handle.join().unwrap());
        enough_cancellation_destroy(ptr);
    }
}

#[test]
fn ffi_multiple_tokens() {
    unsafe {
        let ptr = enough_cancellation_create();

        // Create multiple tokens from same source
        let token1 = FfiCancellationToken::from_ptr(ptr);
        let token2 = FfiCancellationToken::from_ptr(ptr);
        let token3 = FfiCancellationToken::from_ptr(ptr);

        assert!(!token1.is_stopped());
        assert!(!token2.is_stopped());
        assert!(!token3.is_stopped());

        enough_cancellation_cancel(ptr);

        assert!(token1.is_stopped());
        assert!(token2.is_stopped());
        assert!(token3.is_stopped());

        enough_cancellation_destroy(ptr);
    }
}

#[test]
fn ffi_token_is_copy() {
    unsafe {
        let ptr = enough_cancellation_create();
        let token = FfiCancellationToken::from_ptr(ptr);

        let copy = token; // Copy
        let _ = token; // Original still valid
        let _ = copy;

        enough_cancellation_destroy(ptr);
    }
}

#[test]
fn ffi_with_stop_trait() {
    fn use_stop(stop: impl Stop) -> bool {
        stop.is_stopped()
    }

    unsafe {
        let ptr = enough_cancellation_create();
        let token = FfiCancellationToken::from_ptr(ptr);

        assert!(!use_stop(token));

        enough_cancellation_cancel(ptr);

        assert!(use_stop(token));

        enough_cancellation_destroy(ptr);
    }
}

#[test]
fn ffi_simulated_csharp_pattern() {
    // This simulates how C# would use the FFI
    unsafe {
        // C# creates handle
        let handle = enough_cancellation_create();

        // C# registers callback on CancellationToken.Register()
        // In real code, this would be called from a callback
        let cancel_fn = move || {
            enough_cancellation_cancel(handle);
        };

        // Rust library creates token from handle
        let token = FfiCancellationToken::from_ptr(handle);

        // Rust library uses token
        fn process(data: &[u8], stop: impl Stop) -> Result<usize, &'static str> {
            for (i, chunk) in data.chunks(10).enumerate() {
                if i % 100 == 0 && stop.is_stopped() {
                    return Err("cancelled");
                }
            }
            Ok(data.len())
        }

        // Not cancelled yet
        let result = process(&[0u8; 1000], token);
        assert!(result.is_ok());

        // Simulate C# calling cancel (from CancellationToken.Register callback)
        cancel_fn();

        // Now cancelled
        let result = process(&[0u8; 1000], token);
        assert_eq!(result, Err("cancelled"));

        // C# destroys handle
        enough_cancellation_destroy(handle);
    }
}

#[test]
fn ffi_interop_with_enough_std() {
    // Test that FFI tokens and std tokens work together
    use enough::CancellationSource;

    let std_source = CancellationSource::new();
    let std_token = std_source.token();

    unsafe {
        let ffi_ptr = enough_cancellation_create();
        let ffi_token = FfiCancellationToken::from_ptr(ffi_ptr);

        // Both implement Stop
        fn use_any_stop(s: impl Stop) -> bool {
            s.is_stopped()
        }

        assert!(!use_any_stop(std_token.clone()));
        assert!(!use_any_stop(ffi_token));

        std_source.cancel();
        enough_cancellation_cancel(ffi_ptr);

        assert!(use_any_stop(std_token));
        assert!(use_any_stop(ffi_token));

        enough_cancellation_destroy(ffi_ptr);
    }
}

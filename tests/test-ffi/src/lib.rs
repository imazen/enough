//! Tests for FFI helpers.
#![allow(unused_imports, dead_code)]

use almost_enough::Stop;
use enough_ffi::{
    FfiCancellationSource, FfiCancellationToken, FfiCancellationTokenView,
    enough_cancellation_cancel, enough_cancellation_create, enough_cancellation_destroy,
    enough_cancellation_is_cancelled, enough_token_create, enough_token_create_never,
    enough_token_destroy, enough_token_is_cancelled,
};
use std::thread;

#[test]
fn ffi_lifecycle() {
    unsafe {
        let source = enough_cancellation_create();
        assert!(!source.is_null());
        assert!(!enough_cancellation_is_cancelled(source));

        let token = enough_token_create(source);
        assert!(!token.is_null());
        assert!(!enough_token_is_cancelled(token));

        enough_cancellation_cancel(source);
        assert!(enough_cancellation_is_cancelled(source));
        assert!(enough_token_is_cancelled(token));

        enough_token_destroy(token);
        enough_cancellation_destroy(source);
    }
}

#[test]
fn ffi_token_view_from_pointer() {
    unsafe {
        let source = enough_cancellation_create();
        let token = enough_token_create(source);

        // Rust FFI code receives the token pointer and creates a view
        let view = FfiCancellationToken::from_ptr(token);

        assert!(!view.should_stop());

        enough_cancellation_cancel(source);

        assert!(view.should_stop());

        enough_token_destroy(token);
        enough_cancellation_destroy(source);
    }
}

#[test]
fn ffi_null_safety() {
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

        // Null token view is safe
        let view = FfiCancellationToken::from_ptr(std::ptr::null());
        assert!(!view.should_stop());
    }
}

#[test]
fn ffi_never_token() {
    unsafe {
        let token = enough_token_create_never();
        assert!(!enough_token_is_cancelled(token));

        let view = FfiCancellationToken::from_ptr(token);
        assert!(!view.should_stop());
        assert!(view.check().is_ok());

        enough_token_destroy(token);
    }
}

#[test]
fn ffi_cross_thread() {
    unsafe {
        let source = enough_cancellation_create();
        let token = enough_token_create(source);

        // Send token pointer to another thread
        let token_addr = token as usize;
        let handle = thread::spawn(move || {
            let token = token_addr as *const FfiCancellationToken;
            let view = FfiCancellationToken::from_ptr(token);

            // Wait for cancellation
            while !view.should_stop() {
                thread::yield_now();
            }

            true
        });

        // Cancel from main thread
        thread::sleep(std::time::Duration::from_millis(10));
        enough_cancellation_cancel(source);

        assert!(handle.join().unwrap());

        enough_token_destroy(token);
        enough_cancellation_destroy(source);
    }
}

#[test]
fn ffi_multiple_tokens() {
    unsafe {
        let source = enough_cancellation_create();

        // Create multiple tokens from same source
        let token1 = enough_token_create(source);
        let token2 = enough_token_create(source);
        let token3 = enough_token_create(source);

        assert!(!enough_token_is_cancelled(token1));
        assert!(!enough_token_is_cancelled(token2));
        assert!(!enough_token_is_cancelled(token3));

        enough_cancellation_cancel(source);

        assert!(enough_token_is_cancelled(token1));
        assert!(enough_token_is_cancelled(token2));
        assert!(enough_token_is_cancelled(token3));

        enough_token_destroy(token1);
        enough_token_destroy(token2);
        enough_token_destroy(token3);
        enough_cancellation_destroy(source);
    }
}

#[test]
fn ffi_token_survives_source_destruction() {
    unsafe {
        let source = enough_cancellation_create();
        enough_cancellation_cancel(source);

        let token = enough_token_create(source);

        // Destroy source while token exists - NOW SAFE with Arc!
        enough_cancellation_destroy(source);

        // Token should still report cancelled (Arc keeps state alive)
        assert!(enough_token_is_cancelled(token));

        enough_token_destroy(token);
    }
}

#[test]
fn ffi_token_from_destroyed_uncancelled_source() {
    unsafe {
        let source = enough_cancellation_create();
        let token = enough_token_create(source);

        // Destroy source without cancelling
        enough_cancellation_destroy(source);

        // Token remains valid but will never become cancelled
        // (no one can call cancel anymore since source is gone)
        assert!(!enough_token_is_cancelled(token));

        enough_token_destroy(token);
    }
}

#[test]
fn ffi_with_stop_trait() {
    fn use_stop(stop: impl Stop) -> bool {
        stop.should_stop()
    }

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
fn ffi_simulated_csharp_pattern() {
    // This simulates how C# would use the FFI
    unsafe {
        // C# creates source
        let source = enough_cancellation_create();

        // C# creates token for passing to Rust
        let token = enough_token_create(source);

        // C# registers callback on CancellationToken.Register()
        // In real code, this would be called from a callback
        let source_for_cancel = source;
        let cancel_fn = move || {
            enough_cancellation_cancel(source_for_cancel);
        };

        // Rust library receives token pointer and creates a view
        let view = FfiCancellationToken::from_ptr(token);

        // Rust library uses view
        fn process(data: &[u8], stop: impl Stop) -> Result<usize, &'static str> {
            for (i, _chunk) in data.chunks(10).enumerate() {
                if i % 100 == 0 && stop.should_stop() {
                    return Err("cancelled");
                }
            }
            Ok(data.len())
        }

        // Not cancelled yet
        let result = process(&[0u8; 1000], view);
        assert!(result.is_ok());

        // Simulate C# calling cancel (from CancellationToken.Register callback)
        cancel_fn();

        // Now cancelled
        let result = process(&[0u8; 1000], view);
        assert_eq!(result, Err("cancelled"));

        // C# cleans up
        enough_token_destroy(token);
        enough_cancellation_destroy(source);
    }
}

#[test]
fn ffi_interop_with_enough() {
    // Test that FFI tokens and enough tokens work together
    use almost_enough::Stopper;

    let std_stop = Stopper::new();

    unsafe {
        let ffi_source = enough_cancellation_create();
        let ffi_token = enough_token_create(ffi_source);
        let ffi_view = FfiCancellationToken::from_ptr(ffi_token);

        // Both implement Stop
        fn use_any_stop(s: impl Stop) -> bool {
            s.should_stop()
        }

        assert!(!use_any_stop(std_stop.clone()));
        assert!(!use_any_stop(ffi_view));

        std_stop.cancel();
        enough_cancellation_cancel(ffi_source);

        assert!(use_any_stop(std_stop));
        assert!(use_any_stop(ffi_view));

        enough_token_destroy(ffi_token);
        enough_cancellation_destroy(ffi_source);
    }
}

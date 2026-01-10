//! Tests for AtomicBool-based CancellationSource and CancellationToken.

use enough::Stop;
use enough_std::{CancellationSource, CancellationToken};
use std::sync::Arc;
use std::thread;

#[test]
fn source_basic_usage() {
    let source = CancellationSource::new();
    assert!(!source.is_cancelled());

    let token = source.token();
    assert!(!token.is_stopped());

    source.cancel();

    assert!(source.is_cancelled());
    assert!(token.is_stopped());
}

#[test]
fn token_is_copy() {
    let source = CancellationSource::new();
    let t1 = source.token();
    let t2 = t1; // Copy
    let t3 = t1; // Still valid

    source.cancel();

    assert!(t1.is_stopped());
    assert!(t2.is_stopped());
    assert!(t3.is_stopped());
}

#[test]
fn multiple_tokens_same_source() {
    let source = CancellationSource::new();
    let tokens: Vec<_> = (0..100).map(|_| source.token()).collect();

    assert!(tokens.iter().all(|t| !t.is_stopped()));

    source.cancel();

    assert!(tokens.iter().all(|t| t.is_stopped()));
}

#[test]
fn cross_thread_cancellation() {
    let source = Arc::new(CancellationSource::new());
    let token = source.token();

    let source_clone = Arc::clone(&source);
    let handle = thread::spawn(move || {
        thread::sleep(std::time::Duration::from_millis(10));
        source_clone.cancel();
    });

    // Spin until cancelled
    while !token.is_stopped() {
        thread::yield_now();
    }

    handle.join().unwrap();
    assert!(token.is_stopped());
}

#[test]
fn concurrent_check_and_cancel() {
    let source = Arc::new(CancellationSource::new());
    let token = source.token();

    let handles: Vec<_> = (0..10)
        .map(|i| {
            let source = Arc::clone(&source);
            let token = token;
            thread::spawn(move || {
                for _ in 0..1000 {
                    if i == 0 && !source.is_cancelled() {
                        source.cancel();
                    }
                    let _ = token.check();
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    assert!(source.is_cancelled());
}

#[test]
fn never_token() {
    let token = CancellationToken::never();
    assert!(!token.is_stopped());

    // Even with many checks, never stops
    for _ in 0..10000 {
        assert!(token.check().is_ok());
    }
}

#[test]
fn source_reset() {
    let source = CancellationSource::new();
    source.cancel();
    assert!(source.is_cancelled());

    source.reset();
    assert!(!source.is_cancelled());

    // Token created after reset should not be cancelled
    let token = source.token();
    assert!(!token.is_stopped());
}

#[test]
fn pass_to_function() {
    fn process(data: &[u8], stop: impl Stop) -> Result<usize, &'static str> {
        let mut count = 0;
        for chunk in data.chunks(10) {
            if stop.is_stopped() {
                return Err("cancelled");
            }
            count += chunk.len();
        }
        Ok(count)
    }

    let source = CancellationSource::new();
    let token = source.token();

    // Not cancelled - completes
    let result = process(&[0u8; 100], token);
    assert_eq!(result, Ok(100));

    // Cancel and try again
    source.cancel();
    let result = process(&[0u8; 100], token);
    assert_eq!(result, Err("cancelled"));
}

#[test]
fn dyn_stop_usage() {
    let source = CancellationSource::new();
    let token = source.token();

    fn takes_dyn(stop: &dyn Stop) -> bool {
        stop.is_stopped()
    }

    assert!(!takes_dyn(&token));
    source.cancel();
    assert!(takes_dyn(&token));
}

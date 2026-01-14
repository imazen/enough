//! Tests for Stopper.
#![allow(unused_imports, dead_code)]

use almost_enough::{Stop, Stopper};
use std::sync::Arc;
use std::thread;

#[test]
fn source_basic_usage() {
    let source = Stopper::new();
    assert!(!source.is_cancelled());

    let token = source.clone();
    assert!(!token.should_stop());

    source.cancel();

    assert!(source.is_cancelled());
    assert!(token.should_stop());
}

#[test]
fn token_is_clone() {
    let source = Stopper::new();
    let t1 = source.clone();
    let t2 = t1.clone();
    let t3 = t1.clone();

    source.cancel();

    assert!(t1.should_stop());
    assert!(t2.should_stop());
    assert!(t3.should_stop());
}

#[test]
fn multiple_tokens_same_source() {
    let source = Stopper::new();
    let tokens: Vec<Stopper> = (0..100).map(|_| source.clone()).collect();

    assert!(tokens.iter().all(|t| !t.should_stop()));

    source.cancel();

    assert!(tokens.iter().all(|t| t.should_stop()));
}

#[test]
fn cross_thread_cancellation() {
    let source = Stopper::new();
    let token = source.clone();

    let source_clone = source.clone();
    let handle = thread::spawn(move || {
        thread::sleep(std::time::Duration::from_millis(10));
        source_clone.cancel();
    });

    // Spin until cancelled
    while !token.should_stop() {
        thread::yield_now();
    }

    handle.join().unwrap();
    assert!(token.should_stop());
}

#[test]
fn concurrent_check_and_cancel() {
    let source = Stopper::new();
    let token = source.clone();

    let handles: Vec<_> = (0..10)
        .map(|i| {
            let source = source.clone();
            let token = token.clone();
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
fn unstoppable_never_stops() {
    use almost_enough::Unstoppable;
    let stop = Unstoppable;
    assert!(!stop.should_stop());

    // Even with many checks, never stops
    for _ in 0..10000 {
        assert!(stop.check().is_ok());
    }
}

#[test]
fn pass_to_function() {
    fn process(data: &[u8], stop: impl Stop) -> Result<usize, &'static str> {
        let mut count = 0;
        for chunk in data.chunks(10) {
            if stop.should_stop() {
                return Err("cancelled");
            }
            count += chunk.len();
        }
        Ok(count)
    }

    let source = Stopper::new();
    let token = source.clone();

    // Not cancelled - completes
    let result = process(&[0u8; 100], token.clone());
    assert_eq!(result, Ok(100));

    // Cancel and try again
    source.cancel();
    let result = process(&[0u8; 100], token);
    assert_eq!(result, Err("cancelled"));
}

#[test]
fn dyn_stop_usage() {
    let source = Stopper::new();
    let token = source.clone();

    fn takes_dyn(stop: &dyn Stop) -> bool {
        stop.should_stop()
    }

    assert!(!takes_dyn(&token));
    source.cancel();
    assert!(takes_dyn(&token));
}

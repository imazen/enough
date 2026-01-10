//! Tests for rayon parallel processing with cancellation.
#![allow(unused_imports, dead_code)]

use enough::{CancellationSource, Stop, StopReason};
use rayon::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Simulated work that respects cancellation
fn process_item(item: usize, stop: &impl Stop) -> Result<usize, StopReason> {
    // Check cancellation every item
    stop.check()?;
    // Simulate some work
    Ok(item * 2)
}

#[test]
fn parallel_iter_with_token() {
    let source = CancellationSource::new();
    let token = source.token();

    let items: Vec<usize> = (0..1000).collect();

    let results: Vec<_> = items
        .par_iter()
        .map(|&item| process_item(item, &token))
        .collect();

    // All should succeed
    assert!(results.iter().all(|r| r.is_ok()));
}

#[test]
fn parallel_iter_cancelled() {
    let source = Arc::new(CancellationSource::new());
    let token = source.token();
    let processed = Arc::new(AtomicUsize::new(0));

    let items: Vec<usize> = (0..10000).collect();

    // Cancel after processing starts
    let source_clone = Arc::clone(&source);
    let processed_clone = Arc::clone(&processed);
    std::thread::spawn(move || {
        // Wait until some items are processed
        while processed_clone.load(Ordering::Relaxed) < 100 {
            std::thread::yield_now();
        }
        source_clone.cancel();
    });

    let results: Vec<_> = items
        .par_iter()
        .map(|&item| {
            processed.fetch_add(1, Ordering::Relaxed);
            process_item(item, &token)
        })
        .collect();

    // Some should have failed with Cancelled
    let cancelled_count = results
        .iter()
        .filter(|r| matches!(r, Err(StopReason::Cancelled)))
        .count();

    assert!(cancelled_count > 0, "Some items should have been cancelled");

    // But not all (some completed before cancellation)
    let success_count = results.iter().filter(|r| r.is_ok()).count();
    assert!(success_count > 0, "Some items should have succeeded");
}

#[test]
fn parallel_chunks_with_token() {
    let source = CancellationSource::new();
    let token = source.token();

    let data: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();

    let results: Vec<_> = data
        .par_chunks(100)
        .map(|chunk| {
            token.check()?;
            Ok::<_, StopReason>(chunk.iter().map(|&b| b as usize).sum::<usize>())
        })
        .collect();

    assert!(results.iter().all(|r| r.is_ok()));
}

#[test]
fn parallel_find_with_early_exit() {
    let source = CancellationSource::new();
    let token = source.token();
    let checked = Arc::new(AtomicUsize::new(0));

    let items: Vec<usize> = (0..10000).collect();
    let checked_clone = Arc::clone(&checked);

    let found = items.par_iter().find_any(|&&item| {
        checked_clone.fetch_add(1, Ordering::Relaxed);

        // Check cancellation
        if token.is_stopped() {
            return false;
        }

        // Cancel when we find target
        if item == 500 {
            source.cancel();
            return true;
        }

        false
    });

    assert_eq!(found, Some(&500));

    // Should not have checked all items (early exit)
    let total_checked = checked.load(Ordering::Relaxed);
    assert!(
        total_checked < 10000,
        "Should early exit, but checked {}",
        total_checked
    );
}

#[test]
fn token_is_send_sync_for_rayon() {
    // This test just ensures the token can be used with rayon
    // The fact that it compiles is the test

    let source = CancellationSource::new();
    let token = source.token();

    // Token must be Send + Sync for par_iter
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<enough::CancellationToken>();

    let _: Vec<_> = (0..100)
        .into_par_iter()
        .map(|i| {
            let _ = token.check();
            i
        })
        .collect();
}

#[test]
fn nested_parallel_with_token() {
    let source = CancellationSource::new();
    let token = source.token();

    let outer: Vec<Vec<usize>> = (0..10).map(|i| (i * 10..(i + 1) * 10).collect()).collect();

    let result: usize = outer
        .par_iter()
        .map(|inner| {
            if token.is_stopped() {
                return 0;
            }
            inner.par_iter().map(|&x| x).sum::<usize>()
        })
        .sum();

    // Sum of 0..100
    assert_eq!(result, 4950);
}

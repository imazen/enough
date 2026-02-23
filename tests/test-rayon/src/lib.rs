//! Tests for rayon parallel processing with cancellation.
#![allow(unused_imports, dead_code)]

use almost_enough::{Stop, StopReason, Stopper};
use rayon::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Simulated work that respects cancellation
fn process_item(item: usize, stop: &impl Stop) -> Result<usize, StopReason> {
    // Check cancellation every item
    stop.check()?;
    // Simulate some work
    Ok(item * 2)
}

#[test]
fn parallel_iter_with_token() {
    let stop = Stopper::new();

    let items: Vec<usize> = (0..1000).collect();

    let results: Vec<_> = items
        .par_iter()
        .map(|&item| process_item(item, &stop))
        .collect();

    // All should succeed
    assert!(results.iter().all(|r| r.is_ok()));
}

/// This test verifies that cancellation propagates during parallel iteration.
/// Uses synchronization barriers and artificial delays to make behavior deterministic.
#[test]
fn parallel_iter_cancelled() {
    use std::sync::Barrier;

    let stop = Stopper::new();
    // Barrier ensures cancellation thread waits until processing has started
    let barrier = Arc::new(Barrier::new(2));

    // Use enough items that some will definitely be pending when cancellation occurs
    let items: Vec<usize> = (0..10000).collect();

    // Track which item triggered the barrier
    let barrier_item = Arc::new(AtomicUsize::new(usize::MAX));

    // Cancel after first item signals it's processing
    let stop_clone = stop.clone();
    let barrier_clone = Arc::clone(&barrier);
    std::thread::spawn(move || {
        // Wait for processing to signal it has started
        barrier_clone.wait();
        stop_clone.cancel();
    });

    let stop_for_map = stop.clone();
    let barrier_for_map = Arc::clone(&barrier);
    let first_signal = AtomicBool::new(false);
    let first_signal_ref = &first_signal;
    let barrier_item_ref = Arc::clone(&barrier_item);

    let results: Vec<_> = items
        .par_iter()
        .map(|&item| {
            // First item signals barrier and is guaranteed to succeed (doesn't check stop)
            if !first_signal_ref.swap(true, Ordering::Relaxed) {
                barrier_item_ref.store(item, Ordering::Relaxed);
                barrier_for_map.wait();
                // The first item succeeds unconditionally - it's our "before cancellation" reference
                return Ok(item * 2);
            }
            // Small delay per item to ensure cancellation has time to be observed
            std::hint::black_box(item);
            for _ in 0..100 {
                std::hint::black_box(item);
            }
            process_item(item, &stop_for_map)
        })
        .collect();

    // Some should have failed with Cancelled
    let cancelled_count = results
        .iter()
        .filter(|r| matches!(r, Err(StopReason::Cancelled)))
        .count();

    let success_count = results.iter().filter(|r| r.is_ok()).count();

    // All items must be accounted for
    assert_eq!(
        cancelled_count + success_count,
        results.len(),
        "All items should be either cancelled or successful"
    );

    // The first item that triggered the barrier should have succeeded
    assert!(
        success_count >= 1,
        "At least the barrier-triggering item should have succeeded. Got {} successes.",
        success_count
    );

    // CRITICAL: Verify cancellation actually propagated to remaining items
    // With barrier synchronization, cancellation MUST be observed by other items
    assert!(
        cancelled_count > 0,
        "Cancellation should have been observed. Got {} cancelled, {} successful out of {}. \
         This indicates the Stop trait may not be working correctly.",
        cancelled_count,
        success_count,
        results.len()
    );
}

#[test]
fn parallel_chunks_with_token() {
    let stop = Stopper::new();

    let data: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();

    let results: Vec<_> = data
        .par_chunks(100)
        .map(|chunk| {
            stop.check()?;
            Ok::<_, StopReason>(chunk.iter().map(|&b| b as usize).sum::<usize>())
        })
        .collect();

    assert!(results.iter().all(|r| r.is_ok()));
}

#[test]
fn parallel_find_with_early_exit() {
    let stop = Stopper::new();
    let checked = Arc::new(AtomicUsize::new(0));

    let items: Vec<usize> = (0..10000).collect();
    let checked_clone = Arc::clone(&checked);
    let stop_clone = stop.clone();

    let found = items.par_iter().find_any(|&&item| {
        checked_clone.fetch_add(1, Ordering::Relaxed);

        // Check cancellation
        if stop.should_stop() {
            return false;
        }

        // Cancel when we find target
        if item == 500 {
            stop_clone.cancel();
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
fn stopper_is_send_sync_for_rayon() {
    // This test just ensures the stopper can be used with rayon
    // The fact that it compiles is the test

    let stop = Stopper::new();

    // Stopper must be Send + Sync for par_iter
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Stopper>();

    let _: Vec<_> = (0..100)
        .into_par_iter()
        .map(|i| {
            let _ = stop.check();
            i
        })
        .collect();
}

#[test]
fn nested_parallel_with_token() {
    let stop = Stopper::new();

    let outer: Vec<Vec<usize>> = (0..10).map(|i| (i * 10..(i + 1) * 10).collect()).collect();

    let result: usize = outer
        .par_iter()
        .map(|inner| {
            if stop.should_stop() {
                return 0;
            }
            inner.par_iter().map(|&x| x).sum::<usize>()
        })
        .sum();

    // Sum of 0..100
    assert_eq!(result, 4950);
}

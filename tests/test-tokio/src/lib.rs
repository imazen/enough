//! Tests for tokio integration.
#![allow(unused_imports, dead_code)]

use enough::Stop;
use enough_tokio::{CancellationTokenExt, TokioStop};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn tokio_stop_basic() {
    let token = CancellationToken::new();
    let stop = TokioStop::new(token.clone());

    assert!(!stop.should_stop());

    token.cancel();

    assert!(stop.should_stop());
}

#[tokio::test]
async fn tokio_stop_in_spawn_blocking() {
    let token = CancellationToken::new();
    let stop = TokioStop::new(token.clone());

    let completed = Arc::new(AtomicBool::new(false));
    let completed_clone = Arc::clone(&completed);

    let handle = tokio::task::spawn_blocking(move || {
        for _ in 0..1000 {
            if stop.should_stop() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        completed_clone.store(true, Ordering::SeqCst);
    });

    // Cancel after some time
    tokio::time::sleep(Duration::from_millis(50)).await;
    token.cancel();

    handle.await.unwrap();

    // Should have been cancelled, not completed
    assert!(!completed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn tokio_stop_cancelled_await() {
    let token = CancellationToken::new();
    let stop = TokioStop::new(token.clone());

    let cancel_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();
    });

    stop.cancelled().await;

    cancel_task.await.unwrap();
    assert!(stop.should_stop());
}

#[tokio::test]
async fn tokio_stop_child() {
    let parent = TokioStop::new(CancellationToken::new());
    let child = parent.child();

    assert!(!child.should_stop());

    parent.cancel();

    assert!(child.should_stop());
}

#[tokio::test]
async fn tokio_stop_select() {
    let token = CancellationToken::new();
    let stop = TokioStop::new(token.clone());

    let cancel_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();
    });

    let result = tokio::select! {
        _ = stop.cancelled() => "cancelled",
        _ = tokio::time::sleep(Duration::from_secs(10)) => "timeout",
    };

    cancel_task.await.unwrap();
    assert_eq!(result, "cancelled");
}

#[tokio::test]
async fn tokio_extension_trait() {
    let token = CancellationToken::new();
    let stop = token.as_stop();

    assert!(!stop.should_stop());

    token.cancel();

    assert!(stop.should_stop());
}

#[tokio::test]
async fn tokio_with_enough() {
    // Test that tokio and enough tokens can work together through the Stop trait
    let tokio_token = CancellationToken::new();
    let tokio_stop = TokioStop::new(tokio_token.clone());

    let std_source = enough::ArcStop::new();
    let std_token = std_source.token();

    fn use_stop(stop: impl Stop) -> bool {
        stop.should_stop()
    }

    assert!(!use_stop(tokio_stop.clone()));
    assert!(!use_stop(std_token.clone()));

    tokio_token.cancel();
    std_source.cancel();

    assert!(use_stop(tokio_stop));
    assert!(use_stop(std_token));
}

#[tokio::test]
async fn tokio_concurrent_tasks() {
    let token = CancellationToken::new();
    let completed = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];

    for _ in 0..10 {
        let stop = TokioStop::new(token.clone());
        let completed = Arc::clone(&completed);

        handles.push(tokio::spawn(async move {
            for _ in 0..100 {
                if stop.should_stop() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            completed.fetch_add(1, Ordering::SeqCst);
        }));
    }

    // Cancel after some tasks have started
    tokio::time::sleep(Duration::from_millis(50)).await;
    token.cancel();

    for h in handles {
        h.await.unwrap();
    }

    // Some tasks may have completed, but not all
    let count = completed.load(Ordering::SeqCst);
    assert!(count < 10, "Most tasks should have been cancelled");
}

#[tokio::test]
async fn tokio_stop_debug() {
    let token = CancellationToken::new();
    let stop = TokioStop::new(token.clone());

    let debug = format!("{:?}", stop);
    assert!(debug.contains("TokioStop"));

    token.cancel();

    let debug = format!("{:?}", stop);
    assert!(debug.contains("cancelled"));
}

//! Tests for tokio integration.
#![allow(unused_imports, dead_code)]

use almost_enough::Stop;
use enough_tokio::{CancellationTokenStopExt, TokioStop};
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

    let std_stop = almost_enough::Stopper::new();

    fn use_stop(stop: impl Stop) -> bool {
        stop.should_stop()
    }

    assert!(!use_stop(tokio_stop.clone()));
    assert!(!use_stop(std_stop.clone()));

    tokio_token.cancel();
    std_stop.cancel();

    assert!(use_stop(tokio_stop));
    assert!(use_stop(std_stop));
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

/// Test JoinSet with cancellation - common pattern for managing task groups.
#[tokio::test]
async fn tokio_joinset_with_cancellation() {
    use tokio::task::JoinSet;

    let token = CancellationToken::new();
    let completed = Arc::new(AtomicUsize::new(0));

    let mut set = JoinSet::new();

    for i in 0..5 {
        let stop = TokioStop::new(token.clone());
        let completed = Arc::clone(&completed);

        set.spawn(async move {
            for j in 0..100 {
                if stop.should_stop() {
                    return format!("task {} cancelled at iteration {}", i, j);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            completed.fetch_add(1, Ordering::SeqCst);
            format!("task {} completed", i)
        });
    }

    // Cancel after some tasks have started
    tokio::time::sleep(Duration::from_millis(50)).await;
    token.cancel();

    // Collect all results
    let mut results = vec![];
    while let Some(result) = set.join_next().await {
        results.push(result.unwrap());
    }

    // All tasks should have returned (either cancelled or completed)
    assert_eq!(results.len(), 5);

    // At least some should show cancellation
    let cancelled_count = results.iter().filter(|r| r.contains("cancelled")).count();
    assert!(
        cancelled_count > 0,
        "Expected some tasks to be cancelled, got: {:?}",
        results
    );
}

/// Test spawn_blocking inside a spawned async task - nested sync/async boundary.
#[tokio::test]
async fn tokio_spawn_blocking_inside_spawn() {
    let token = CancellationToken::new();
    let completed = Arc::new(AtomicBool::new(false));
    let completed_clone = Arc::clone(&completed);

    let handle = tokio::spawn(async move {
        let stop = TokioStop::new(token.clone());

        // Spawn blocking from within async task
        let blocking_handle = tokio::task::spawn_blocking(move || {
            for _ in 0..1000 {
                if stop.should_stop() {
                    return "cancelled";
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            "completed"
        });

        blocking_handle.await.unwrap()
    });

    // Spawn a separate task with its own token to test proper nesting
    let nested_token = CancellationToken::new();
    let nested_stop = TokioStop::new(nested_token.clone());

    let nested_handle = tokio::spawn(async move {
        let blocking_result = tokio::task::spawn_blocking(move || {
            for _ in 0..1000 {
                if nested_stop.should_stop() {
                    return "inner cancelled";
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            completed_clone.store(true, Ordering::SeqCst);
            "inner completed"
        });

        blocking_result.await.unwrap()
    });

    // Cancel nested after some time
    tokio::time::sleep(Duration::from_millis(50)).await;
    nested_token.cancel();

    let result = nested_handle.await.unwrap();

    // Should have been cancelled
    assert_eq!(result, "inner cancelled");
    assert!(!completed.load(Ordering::SeqCst));

    // Clean up first handle (never cancelled, will complete on its own eventually)
    drop(handle);
}

/// Test graceful shutdown pattern - cancel + await with timeout for cleanup.
#[tokio::test]
async fn tokio_graceful_shutdown_with_timeout() {
    let token = CancellationToken::new();
    let token_clone = token.clone();
    let cleanup_done = Arc::new(AtomicBool::new(false));
    let cleanup_done_clone = Arc::clone(&cleanup_done);

    let handle = tokio::spawn(async move {
        let stop = TokioStop::new(token_clone);

        // Main work loop
        loop {
            if stop.should_stop() {
                // Graceful cleanup phase
                tokio::time::sleep(Duration::from_millis(20)).await;
                cleanup_done_clone.store(true, Ordering::SeqCst);
                return "graceful shutdown";
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });

    // Trigger cancellation
    tokio::time::sleep(Duration::from_millis(30)).await;
    token.cancel();

    // Wait for graceful shutdown with timeout
    let result = tokio::time::timeout(Duration::from_millis(500), handle).await;

    match result {
        Ok(Ok(msg)) => {
            assert_eq!(msg, "graceful shutdown");
            assert!(cleanup_done.load(Ordering::SeqCst));
        }
        Ok(Err(e)) => panic!("Task panicked: {:?}", e),
        Err(_) => panic!("Graceful shutdown timed out"),
    }
}

/// Test nested spawn with child tokens - task hierarchy with independent cancellation.
#[tokio::test]
async fn tokio_nested_spawn_with_child_tokens() {
    let root_token = CancellationToken::new();
    let root_stop = TokioStop::new(root_token.clone());

    let parent_completed = Arc::new(AtomicBool::new(false));
    let child_completed = Arc::new(AtomicBool::new(false));
    let parent_completed_clone = Arc::clone(&parent_completed);
    let child_completed_clone = Arc::clone(&child_completed);

    let parent_handle = tokio::spawn(async move {
        let child_stop = root_stop.child();

        // Spawn a child task with child token
        let child_handle = tokio::spawn(async move {
            for _ in 0..100 {
                if child_stop.should_stop() {
                    return "child cancelled";
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            child_completed_clone.store(true, Ordering::SeqCst);
            "child completed"
        });

        // Parent also does work
        for _ in 0..100 {
            if root_stop.should_stop() {
                // Wait for child to finish
                let child_result = child_handle.await.unwrap();
                return format!("parent cancelled, {}", child_result);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let child_result = child_handle.await.unwrap();
        parent_completed_clone.store(true, Ordering::SeqCst);
        format!("parent completed, {}", child_result)
    });

    // Cancel root after some time
    tokio::time::sleep(Duration::from_millis(50)).await;
    root_token.cancel();

    let result = parent_handle.await.unwrap();

    // Both should be cancelled
    assert!(result.contains("cancelled"), "Result was: {}", result);
    assert!(!parent_completed.load(Ordering::SeqCst));
    assert!(!child_completed.load(Ordering::SeqCst));
}

/// Test child cancellation doesn't affect parent - important for scoped work.
#[tokio::test]
async fn tokio_child_cancellation_isolated() {
    let parent_stop = TokioStop::new(CancellationToken::new());
    let child_stop = parent_stop.child();

    let parent_completed = Arc::new(AtomicBool::new(false));
    let child_completed = Arc::new(AtomicBool::new(false));
    let parent_completed_clone = Arc::clone(&parent_completed);
    let child_completed_clone = Arc::clone(&child_completed);

    // Child task
    let child_stop_clone = child_stop.clone();
    let child_handle = tokio::spawn(async move {
        for _ in 0..100 {
            if child_stop_clone.should_stop() {
                return "child cancelled";
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        child_completed_clone.store(true, Ordering::SeqCst);
        "child completed"
    });

    // Parent task (continues even after child cancellation)
    let parent_stop_clone = parent_stop.clone();
    let parent_handle = tokio::spawn(async move {
        for _ in 0..50 {
            if parent_stop_clone.should_stop() {
                return "parent cancelled";
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        parent_completed_clone.store(true, Ordering::SeqCst);
        "parent completed"
    });

    // Cancel only the child
    tokio::time::sleep(Duration::from_millis(30)).await;
    child_stop.cancel();

    let child_result = child_handle.await.unwrap();
    let parent_result = parent_handle.await.unwrap();

    // Child was cancelled, parent completed
    assert_eq!(child_result, "child cancelled");
    assert_eq!(parent_result, "parent completed");
    assert!(parent_completed.load(Ordering::SeqCst));
    assert!(!child_completed.load(Ordering::SeqCst));
}

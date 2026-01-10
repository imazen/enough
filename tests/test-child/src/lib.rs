//! Tests for child cancellation (hierarchical cancellation trees).
#![allow(unused_imports, dead_code)]

use enough::{CancellationSource, ChildCancellationSource, Stop};

#[test]
fn child_inherits_parent() {
    let parent = CancellationSource::new();
    let child = ChildCancellationSource::new(parent.token());

    assert!(!child.is_cancelled());

    parent.cancel();

    assert!(child.is_cancelled());
    assert!(child.token().is_stopped());
}

#[test]
fn child_cancel_independent() {
    let parent = CancellationSource::new();
    let child = ChildCancellationSource::new(parent.token());

    child.cancel();

    // Child is cancelled
    assert!(child.is_cancelled());
    assert!(child.token().is_stopped());

    // Parent is NOT cancelled
    assert!(!parent.is_cancelled());
}

#[test]
fn siblings_independent() {
    let parent = CancellationSource::new();
    let child_a = ChildCancellationSource::new(parent.token());
    let child_b = ChildCancellationSource::new(parent.token());

    child_a.cancel();

    // A is cancelled
    assert!(child_a.is_cancelled());
    // B is NOT cancelled
    assert!(!child_b.is_cancelled());

    // Parent cancellation affects both
    parent.cancel();
    assert!(child_b.is_cancelled());
}

#[test]
fn grandchild_inherits_all() {
    let grandparent = CancellationSource::new();
    let parent = ChildCancellationSource::new(grandparent.token());
    let child = parent.child();

    assert!(!child.is_cancelled());

    // Grandparent cancel propagates
    grandparent.cancel();
    assert!(child.is_cancelled());
}

#[test]
fn grandchild_parent_cancel() {
    let grandparent = CancellationSource::new();
    let parent = ChildCancellationSource::new(grandparent.token());
    let child = parent.child();

    // Parent cancel propagates to child
    parent.cancel();
    assert!(child.is_cancelled());

    // But grandparent is not affected
    assert!(!grandparent.is_cancelled());
}

#[test]
fn deep_hierarchy() {
    let root = CancellationSource::new();
    let l1 = ChildCancellationSource::new(root.token());
    let l2 = l1.child();
    let l3 = l2.child();
    let l4 = l3.child();
    let l5 = l4.child();

    assert!(!l5.is_cancelled());

    root.cancel();

    assert!(l5.is_cancelled());
}

#[test]
fn self_cancelled_vs_any_cancelled() {
    let parent = CancellationSource::new();
    let child = ChildCancellationSource::new(parent.token());

    // Before any cancellation
    assert!(!child.is_self_cancelled());
    assert!(!child.is_cancelled());

    // Parent cancelled - child inherits but not self-cancelled
    parent.cancel();
    assert!(!child.is_self_cancelled());
    assert!(child.is_cancelled());

    // Now cancel child
    child.cancel();
    assert!(child.is_self_cancelled());
    assert!(child.is_cancelled());
}

#[test]
fn child_reset() {
    let parent = CancellationSource::new();
    let child = ChildCancellationSource::new(parent.token());

    child.cancel();
    assert!(child.is_self_cancelled());

    child.reset();
    assert!(!child.is_self_cancelled());
    assert!(!child.is_cancelled());
}

#[test]
fn child_token_with_timeout() {
    use std::time::Duration;

    let parent = CancellationSource::new();
    let child = ChildCancellationSource::new(parent.token());
    let token = child.token().with_timeout(Duration::from_millis(50));

    assert!(!token.is_stopped());

    std::thread::sleep(Duration::from_millis(100));

    assert!(token.is_stopped());
}

#[test]
fn child_across_threads() {
    use std::sync::Arc;
    use std::thread;

    let parent = Arc::new(CancellationSource::new());
    let parent_clone = Arc::clone(&parent);

    let handle = thread::spawn(move || {
        let child = ChildCancellationSource::new(parent_clone.token());

        // Spin until cancelled
        while !child.is_cancelled() {
            thread::yield_now();
        }

        true
    });

    thread::sleep(std::time::Duration::from_millis(10));
    parent.cancel();

    assert!(handle.join().unwrap());
}

#[test]
fn many_children() {
    let parent = CancellationSource::new();
    let children: Vec<_> = (0..100)
        .map(|_| ChildCancellationSource::new(parent.token()))
        .collect();

    assert!(children.iter().all(|c| !c.is_cancelled()));

    parent.cancel();

    assert!(children.iter().all(|c| c.is_cancelled()));
}

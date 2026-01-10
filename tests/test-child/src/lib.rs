//! Tests for child cancellation (hierarchical cancellation trees).
#![allow(unused_imports, dead_code)]

use enough::children::ChildSource;
use enough::{ArcStop, Stop, TimeoutExt};

#[test]
fn child_inherits_parent() {
    let parent = ArcStop::new();
    let child = ChildSource::new(parent.token());

    assert!(!child.is_cancelled());

    parent.cancel();

    assert!(child.is_cancelled());
    assert!(child.token().should_stop());
}

#[test]
fn child_cancel_independent() {
    let parent = ArcStop::new();
    let child = ChildSource::new(parent.token());

    child.cancel();

    // Child is cancelled
    assert!(child.is_cancelled());
    assert!(child.token().should_stop());

    // Parent is NOT cancelled
    assert!(!parent.is_cancelled());
}

#[test]
fn siblings_independent() {
    let parent = ArcStop::new();
    let child_a = ChildSource::new(parent.token());
    let child_b = ChildSource::new(parent.token());

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
    let grandparent = ArcStop::new();
    let parent = ChildSource::new(grandparent.token());
    let child = parent.child();

    assert!(!child.is_cancelled());

    // Grandparent cancel propagates
    grandparent.cancel();
    assert!(child.is_cancelled());
}

#[test]
fn grandchild_parent_cancel() {
    let grandparent = ArcStop::new();
    let parent = ChildSource::new(grandparent.token());
    let child = parent.child();

    // Parent cancel propagates to child
    parent.cancel();
    assert!(child.is_cancelled());

    // But grandparent is not affected
    assert!(!grandparent.is_cancelled());
}

#[test]
fn deep_hierarchy() {
    let root = ArcStop::new();
    let l1 = ChildSource::new(root.token());
    let l2 = l1.child();
    let l3 = l2.child();
    let l4 = l3.child();
    let l5 = l4.child();

    assert!(!l5.is_cancelled());

    root.cancel();

    assert!(l5.is_cancelled());
}

#[test]
fn child_token_with_timeout() {
    use std::time::Duration;

    let parent = ArcStop::new();
    let child = ChildSource::new(parent.token());
    let token = child.token().with_timeout(Duration::from_millis(50));

    assert!(!token.should_stop());

    std::thread::sleep(Duration::from_millis(100));

    assert!(token.should_stop());
}

#[test]
fn child_across_threads() {
    use std::thread;

    let parent = ArcStop::new();
    let parent_clone = parent.clone();

    let handle = thread::spawn(move || {
        let child = ChildSource::new(parent_clone.token());

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
    let parent = ArcStop::new();
    let children: Vec<_> = (0..100)
        .map(|_| ChildSource::new(parent.token()))
        .collect();

    assert!(children.iter().all(|c| !c.is_cancelled()));

    parent.cancel();

    assert!(children.iter().all(|c| c.is_cancelled()));
}

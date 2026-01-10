//! Tests for timeout behavior.
#![allow(unused_imports, dead_code)]

use enough::{ArcStop, ArcToken, Never, Stop, TimeoutExt, StopReason};
use std::time::{Duration, Instant};

#[test]
fn timeout_expires() {
    let source = ArcStop::new();
    let token = source.token().with_timeout(Duration::from_millis(50));

    assert!(!token.should_stop());

    std::thread::sleep(Duration::from_millis(100));

    assert!(token.should_stop());
    assert_eq!(token.check(), Err(StopReason::TimedOut));
}

#[test]
fn timeout_not_expired() {
    let source = ArcStop::new();
    let token = source.token().with_timeout(Duration::from_secs(60));

    assert!(!token.should_stop());
    assert!(token.check().is_ok());
}

#[test]
fn cancel_before_timeout() {
    let source = ArcStop::new();
    let token = source.token().with_timeout(Duration::from_secs(60));

    source.cancel();

    // Should be Cancelled, not TimedOut
    assert_eq!(token.check(), Err(StopReason::Cancelled));
}

#[test]
fn timeout_tightens_not_loosens() {
    let source = ArcStop::new();

    // Parent: 60 seconds
    let parent = source.token().with_timeout(Duration::from_secs(60));

    // Child: 1 second - should be ~1s, not 61s
    let child = parent.tighten(Duration::from_secs(1));

    let remaining = child.remaining();
    assert!(remaining < Duration::from_secs(2));
    assert!(remaining > Duration::from_millis(500));
}

#[test]
fn multiple_timeouts_take_min() {
    let source = ArcStop::new();

    let token = source
        .token()
        .with_timeout(Duration::from_secs(60))
        .tighten(Duration::from_secs(30))
        .tighten(Duration::from_secs(10))
        .tighten(Duration::from_secs(5));

    let remaining = token.remaining();
    assert!(remaining < Duration::from_secs(6));
    assert!(remaining > Duration::from_secs(4));
}

#[test]
fn absolute_deadline() {
    let source = ArcStop::new();
    let deadline = Instant::now() + Duration::from_millis(50);
    let token = source.token().with_deadline(deadline);

    assert!(!token.should_stop());
    assert_eq!(token.deadline(), deadline);

    std::thread::sleep(Duration::from_millis(100));

    assert!(token.should_stop());
}

#[test]
fn remaining_decreases() {
    let source = ArcStop::new();
    let token = source.token().with_timeout(Duration::from_secs(10));

    let r1 = token.remaining();
    std::thread::sleep(Duration::from_millis(100));
    let r2 = token.remaining();

    assert!(r2 < r1);
}

#[test]
fn never_no_timeout() {
    let stop = Never.with_timeout(Duration::from_secs(60));
    assert!(stop.remaining() < Duration::from_secs(61));
    assert!(!stop.should_stop());
}

#[test]
fn timeout_chain_through_functions() {
    fn level1(stop: impl Stop) -> Result<(), StopReason> {
        // Add 5 second timeout for this level
        let stop = stop.with_timeout(Duration::from_secs(5));
        level2(stop)
    }

    fn level2(stop: impl Stop) -> Result<(), StopReason> {
        // Add 2 second timeout - should be min(parent, 2s)
        let stop = stop.with_timeout(Duration::from_secs(2));
        stop.check()
    }

    let source = ArcStop::new();
    let token = source.token().with_timeout(Duration::from_secs(60));

    let result = level1(token);
    assert!(result.is_ok());
}

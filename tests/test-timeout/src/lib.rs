//! Tests for timeout behavior.

use enough::{CancellationSource, CancellationToken, Stop, StopReason};
use std::time::{Duration, Instant};

#[test]
fn timeout_expires() {
    let source = CancellationSource::new();
    let token = source.token().with_timeout(Duration::from_millis(50));

    assert!(!token.is_stopped());

    std::thread::sleep(Duration::from_millis(100));

    assert!(token.is_stopped());
    assert_eq!(token.check(), Err(StopReason::TimedOut));
}

#[test]
fn timeout_not_expired() {
    let source = CancellationSource::new();
    let token = source.token().with_timeout(Duration::from_secs(60));

    assert!(!token.is_stopped());
    assert!(token.check().is_ok());
}

#[test]
fn cancel_before_timeout() {
    let source = CancellationSource::new();
    let token = source.token().with_timeout(Duration::from_secs(60));

    source.cancel();

    // Should be Cancelled, not TimedOut
    assert_eq!(token.check(), Err(StopReason::Cancelled));
}

#[test]
fn timeout_tightens_not_loosens() {
    let source = CancellationSource::new();

    // Parent: 60 seconds
    let parent = source.token().with_timeout(Duration::from_secs(60));

    // Child: 1 second - should be ~1s, not 61s
    let child = parent.with_timeout(Duration::from_secs(1));

    let remaining = child.remaining().expect("should have deadline");
    assert!(remaining < Duration::from_secs(2));
    assert!(remaining > Duration::from_millis(500));
}

#[test]
fn multiple_timeouts_take_min() {
    let source = CancellationSource::new();

    let token = source
        .token()
        .with_timeout(Duration::from_secs(60))
        .with_timeout(Duration::from_secs(30))
        .with_timeout(Duration::from_secs(10))
        .with_timeout(Duration::from_secs(5));

    let remaining = token.remaining().expect("should have deadline");
    assert!(remaining < Duration::from_secs(6));
    assert!(remaining > Duration::from_secs(4));
}

#[test]
fn absolute_deadline() {
    let source = CancellationSource::new();
    let deadline = Instant::now() + Duration::from_millis(50);
    let token = source.token().with_deadline(deadline);

    assert!(!token.is_stopped());
    assert_eq!(token.deadline(), Some(deadline));

    std::thread::sleep(Duration::from_millis(100));

    assert!(token.is_stopped());
}

#[test]
fn remaining_decreases() {
    let source = CancellationSource::new();
    let token = source.token().with_timeout(Duration::from_secs(10));

    let r1 = token.remaining().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let r2 = token.remaining().unwrap();

    assert!(r2 < r1);
}

#[test]
fn remaining_none_without_deadline() {
    let source = CancellationSource::new();
    let token = source.token();

    assert!(token.remaining().is_none());
    assert!(token.deadline().is_none());
}

#[test]
fn never_token_no_timeout() {
    let token = CancellationToken::never();
    assert!(token.remaining().is_none());
    assert!(!token.is_stopped());
}

#[test]
fn timeout_chain_through_functions() {
    fn level1(stop: CancellationToken) -> Result<(), StopReason> {
        // Add 5 second timeout for this level
        let stop = stop.with_timeout(Duration::from_secs(5));
        level2(stop)
    }

    fn level2(stop: CancellationToken) -> Result<(), StopReason> {
        // Add 2 second timeout - should be min(parent, 2s)
        let stop = stop.with_timeout(Duration::from_secs(2));
        stop.check()
    }

    let source = CancellationSource::new();
    let token = source.token().with_timeout(Duration::from_secs(60));

    let result = level1(token);
    assert!(result.is_ok());
}

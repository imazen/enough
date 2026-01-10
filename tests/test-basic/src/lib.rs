//! Basic tests for the Stop trait and Never type.
#![allow(unused_imports, dead_code)]

use enough::{Never, Stop, StopReason};

/// Mock codec function that accepts impl Stop
fn mock_decode(data: &[u8], stop: impl Stop) -> Result<Vec<u8>, MockError> {
    let mut output = Vec::new();
    for (i, chunk) in data.chunks(16).enumerate() {
        if i % 4 == 0 {
            stop.check()?;
        }
        output.extend_from_slice(chunk);
    }
    Ok(output)
}

#[derive(Debug, PartialEq)]
enum MockError {
    Stopped(StopReason),
    Other,
}

impl From<StopReason> for MockError {
    fn from(r: StopReason) -> Self {
        MockError::Stopped(r)
    }
}

#[test]
fn never_allows_completion() {
    let data = vec![0u8; 1000];
    let result = mock_decode(&data, Never);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().len(), 1000);
}

#[test]
fn never_is_zero_cost() {
    // This should compile to essentially nothing
    let stop = Never;
    for _ in 0..1000000 {
        assert!(stop.check().is_ok());
    }
}

#[test]
fn stop_trait_object_works() {
    let stop: &dyn Stop = &Never;
    assert!(!stop.should_stop());
    assert!(stop.check().is_ok());
}

#[test]
fn reference_impl_works() {
    let never = Never;
    let reference: &Never = &never;

    fn takes_stop(s: impl Stop) -> bool {
        s.should_stop()
    }

    assert!(!takes_stop(reference));
}

#[test]
fn stop_reason_display() {
    assert_eq!(format!("{}", StopReason::Cancelled), "operation cancelled");
    assert_eq!(format!("{}", StopReason::TimedOut), "operation timed out");
}

#[test]
fn stop_reason_transient() {
    assert!(!StopReason::Cancelled.is_transient());
    assert!(StopReason::TimedOut.is_transient());
}

#[test]
fn stop_reason_predicates() {
    assert!(StopReason::Cancelled.is_cancelled());
    assert!(!StopReason::Cancelled.is_timed_out());

    assert!(!StopReason::TimedOut.is_cancelled());
    assert!(StopReason::TimedOut.is_timed_out());
}

#[cfg(feature = "alloc")]
#[test]
fn box_impl_works() {
    extern crate alloc;
    use alloc::boxed::Box;

    let boxed: Box<dyn Stop> = Box::new(Never);
    assert!(!boxed.should_stop());
}

#[cfg(feature = "alloc")]
#[test]
fn arc_impl_works() {
    extern crate alloc;
    use alloc::sync::Arc;

    let arc: Arc<dyn Stop> = Arc::new(Never);
    assert!(!arc.should_stop());
}

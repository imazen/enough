//! Validation crate for the `zerodep` cooperative-cancellation pattern.
//!
//! This crate exists to test that the [`zerodep`] module — a zero-dep,
//! one-file cancellation pattern — actually works in practice:
//!
//! 1. Standalone (no cancellation dep at all).
//! 2. As a drop-in target for `enough` users (forward adapter).
//! 3. As a client of `enough`-using libraries (reverse adapter).
//!
//! The `zerodep` module is what a crate author would copy-paste into
//! their own crate. This `lib.rs` uses `almost-enough` as a dev-dep
//! only to prove interop works — `zerodep` itself depends on nothing
//! beyond `alloc`.

pub mod zerodep;

// ---------------------------------------------------------------------
// A mock zero-dep decoder. This is what a library that adopts the
// pattern looks like. No `enough` dep, no trait plumbing — just a
// stored `StopCheck` and a `self.stop.check()?` call in the hot loop.
// ---------------------------------------------------------------------

use zerodep::{StopCheck, StopReason};

/// Error type for the mock zero-dep decoder.
///
/// Implements `From<StopReason>` so `self.stop.check()?` works
/// directly and preserves the reason.
#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    Stopped(StopReason),
    Empty,
}

impl From<StopReason> for DecodeError {
    fn from(r: StopReason) -> Self {
        DecodeError::Stopped(r)
    }
}

/// Mock zero-dep decoder. Stores a `StopCheck` — no lifetime
/// parameter, no viral generics. Checks every 16 chunks.
pub struct Decoder {
    stop: StopCheck,
    block_size: usize,
    check_frequency: usize,
}

impl Decoder {
    pub fn new(stop: StopCheck) -> Self {
        Self {
            stop,
            block_size: 64,
            check_frequency: 16,
        }
    }

    pub fn decode(&self, data: &[u8]) -> Result<Vec<u8>, DecodeError> {
        if data.is_empty() {
            return Err(DecodeError::Empty);
        }
        let mut out = Vec::with_capacity(data.len());
        for (i, chunk) in data.chunks(self.block_size).enumerate() {
            if i % self.check_frequency == 0 {
                self.stop.check()?;
            }
            for &byte in chunk {
                out.push(byte.wrapping_add(1));
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------
// Standalone tests — the pattern works with nothing else in scope.
// ---------------------------------------------------------------------

#[cfg(test)]
mod standalone {
    use super::*;
    use core::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn none_passes_through() {
        let decoder = Decoder::new(StopCheck::none());
        let data = vec![0u8; 10_000];
        assert_eq!(decoder.decode(&data).unwrap().len(), 10_000);
    }

    #[test]
    fn none_is_default() {
        let default: StopCheck = Default::default();
        assert!(default.check().is_ok());
        assert!(!default.may_stop());
    }

    #[test]
    fn none_is_const() {
        const NONE: StopCheck = StopCheck::none();
        assert!(NONE.check().is_ok());
    }

    #[test]
    fn from_flag_always_false() {
        let decoder = Decoder::new(StopCheck::from_flag(|| false));
        let data = vec![0u8; 10_000];
        assert_eq!(decoder.decode(&data).unwrap().len(), 10_000);
    }

    #[test]
    fn from_flag_always_true_reports_cancelled() {
        let decoder = Decoder::new(StopCheck::from_flag(|| true));
        let data = vec![0u8; 10_000];
        assert_eq!(
            decoder.decode(&data).unwrap_err(),
            DecodeError::Stopped(StopReason::Cancelled)
        );
    }

    #[test]
    fn new_with_explicit_reason() {
        let decoder = Decoder::new(StopCheck::new(|| Err(StopReason::TimedOut)));
        let data = vec![0u8; 10_000];
        assert_eq!(
            decoder.decode(&data).unwrap_err(),
            DecodeError::Stopped(StopReason::TimedOut)
        );
    }

    #[test]
    fn from_atomic_bridge() {
        let flag = Arc::new(AtomicBool::new(false));
        let stop = StopCheck::from_atomic(flag.clone());
        let decoder = Decoder::new(stop);
        let data = vec![0u8; 10_000];

        assert!(decoder.decode(&data).is_ok());

        flag.store(true, Ordering::Relaxed);
        assert_eq!(
            decoder.decode(&data).unwrap_err(),
            DecodeError::Stopped(StopReason::Cancelled)
        );
    }

    #[test]
    fn from_atomic_clone_shares_state() {
        let flag = Arc::new(AtomicBool::new(false));
        let stop = StopCheck::from_atomic(flag.clone());
        let clone = stop.clone();

        assert!(stop.check().is_ok());
        assert!(clone.check().is_ok());

        flag.store(true, Ordering::Relaxed);
        assert!(stop.check().is_err());
        assert!(clone.check().is_err());
    }

    #[test]
    fn question_mark_ergonomics() {
        fn worker(stop: &StopCheck) -> Result<(), DecodeError> {
            for _ in 0..1000 {
                stop.check()?;
            }
            Ok(())
        }

        assert!(worker(&StopCheck::none()).is_ok());

        let always = StopCheck::from_flag(|| true);
        assert_eq!(
            worker(&always).unwrap_err(),
            DecodeError::Stopped(StopReason::Cancelled)
        );
    }

    #[test]
    fn stop_check_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StopCheck>();
        assert_send_sync::<StopReason>();
    }

    #[test]
    fn stop_check_is_clone_via_arc() {
        let flag = Arc::new(AtomicBool::new(false));
        let stop = StopCheck::from_atomic(flag.clone());

        let clone1 = stop.clone();
        let clone2 = stop.clone();

        assert!(clone1.check().is_ok());
        assert!(clone2.check().is_ok());

        flag.store(true, Ordering::Relaxed);
        assert!(stop.check().is_err());
        assert!(clone1.check().is_err());
        assert!(clone2.check().is_err());
    }

    #[test]
    fn stop_check_clone_crosses_thread_spawn() {
        use std::thread;

        let flag = Arc::new(AtomicBool::new(false));
        let stop = StopCheck::from_atomic(flag.clone());

        let stop_bg = stop.clone();
        let data = Arc::new(vec![0u8; 1_000_000]);
        let data_bg = Arc::clone(&data);

        let handle = thread::spawn(move || {
            let decoder = Decoder::new(stop_bg);
            for _ in 0..10_000 {
                match decoder.decode(&data_bg[..100]) {
                    Ok(_) => continue,
                    Err(DecodeError::Stopped(r)) => return Err(r),
                    Err(e) => panic!("unexpected: {:?}", e),
                }
            }
            Ok(())
        });

        flag.store(true, Ordering::Relaxed);
        assert_eq!(handle.join().unwrap(), Err(StopReason::Cancelled));
    }

    #[test]
    fn stored_in_struct_without_lifetime() {
        fn take_decoder(_: Decoder) {}
        fn return_decoder() -> Decoder {
            Decoder::new(StopCheck::none())
        }
        take_decoder(return_decoder());
    }

    #[test]
    fn may_stop_semantics() {
        assert!(!StopCheck::none().may_stop());
        assert!(StopCheck::from_flag(|| false).may_stop());
        assert!(StopCheck::from_flag(|| true).may_stop());
        assert!(StopCheck::new(|| Ok(())).may_stop());
        assert!(StopCheck::new(|| Err(StopReason::Cancelled)).may_stop());
        assert!(StopCheck::from_atomic(Arc::new(AtomicBool::new(false))).may_stop());
    }

    #[test]
    fn maybe_none_is_none() {
        let stop = StopCheck::maybe(None::<fn() -> Result<(), StopReason>>);
        assert!(!stop.may_stop());
    }

    #[test]
    fn maybe_flag_none_is_none() {
        let stop = StopCheck::maybe_flag(None::<fn() -> bool>);
        assert!(!stop.may_stop());
    }

    #[test]
    fn debug_impl_reports_may_stop() {
        let dbg_none = format!("{:?}", StopCheck::none());
        assert!(dbg_none.contains("false"));

        let dbg_some = format!("{:?}", StopCheck::from_flag(|| false));
        assert!(dbg_some.contains("true"));
    }

    #[test]
    fn stop_reason_impls_core_error() {
        fn assert_error<T: core::error::Error>() {}
        assert_error::<StopReason>();

        let r: StopReason = StopReason::Cancelled;
        let e: &dyn core::error::Error = &r;
        assert!(e.source().is_none());
    }

    #[test]
    fn stop_reason_display() {
        assert_eq!(format!("{}", StopReason::Cancelled), "operation cancelled");
        assert_eq!(format!("{}", StopReason::TimedOut), "operation timed out");
    }

    #[test]
    fn stop_reason_match_directly() {
        #[allow(unreachable_patterns)]
        fn classify(r: StopReason) -> &'static str {
            match r {
                StopReason::Cancelled => "cancelled",
                StopReason::TimedOut => "timed_out",
                _ => "unknown",
            }
        }
        assert_eq!(classify(StopReason::Cancelled), "cancelled");
        assert_eq!(classify(StopReason::TimedOut), "timed_out");
    }
}

// ---------------------------------------------------------------------
// Forward adapter: an `enough` user calls a `zerodep` library.
// ---------------------------------------------------------------------

#[cfg(test)]
mod forward_adapter {
    use super::*;
    use almost_enough::{Stop, Stopper, TimeoutExt, Unstoppable};

    use almost_enough::StopReason as EReason;
    use zerodep::StopReason as ZReason;

    #[allow(unreachable_patterns)]
    fn map_reason(r: EReason) -> ZReason {
        match r {
            EReason::Cancelled => ZReason::Cancelled,
            EReason::TimedOut => ZReason::TimedOut,
            _ => ZReason::Cancelled,
        }
    }

    #[test]
    fn unstoppable_collapses_to_none() {
        let stop = Unstoppable;
        // Unstoppable is Copy/zero-sized, so the closure is trivial.
        // For Stopper (which needs a clone), use .then(|| { ... }).
        fn never() -> bool {
            false
        }
        let check = StopCheck::maybe_flag(stop.may_stop().then_some(never as fn() -> bool));
        assert!(!check.may_stop());

        let decoder = Decoder::new(check);
        let data = vec![0u8; 10_000];
        assert!(decoder.decode(&data).is_ok());
    }

    #[test]
    fn stopper_bridges_via_maybe_flag() {
        let stop = Stopper::new();
        let check = StopCheck::maybe_flag(stop.may_stop().then(|| {
            let s = stop.clone();
            move || s.should_stop() // enough's should_stop
        }));
        assert!(check.may_stop());

        let decoder = Decoder::new(check);
        let data = vec![0u8; 10_000];
        assert!(decoder.decode(&data).is_ok());

        stop.cancel();
        assert_eq!(
            decoder.decode(&data).unwrap_err(),
            DecodeError::Stopped(ZReason::Cancelled)
        );
    }

    #[test]
    fn reason_preserving_bridge_from_enough() {
        let stop = Stopper::new();
        let timed = stop
            .clone()
            .with_timeout(core::time::Duration::from_secs(60));
        let check = StopCheck::maybe(timed.may_stop().then(|| {
            let t = timed.clone();
            move || t.check().map_err(map_reason)
        }));

        let decoder = Decoder::new(check);
        let data = vec![0u8; 10_000];

        assert!(decoder.decode(&data).is_ok());

        stop.cancel();
        assert_eq!(
            decoder.decode(&data).unwrap_err(),
            DecodeError::Stopped(ZReason::Cancelled),
        );
    }

    #[test]
    fn reason_preserving_bridge_timed_out() {
        use std::thread;
        use std::time::Duration;

        let stop = Stopper::new();
        let timed = stop.clone().with_timeout(Duration::from_millis(1));
        let check = StopCheck::maybe(timed.may_stop().then(|| {
            let t = timed.clone();
            move || t.check().map_err(map_reason)
        }));

        let decoder = Decoder::new(check);
        let data = vec![0u8; 10_000];

        thread::sleep(Duration::from_millis(10));

        assert_eq!(
            decoder.decode(&data).unwrap_err(),
            DecodeError::Stopped(ZReason::TimedOut),
        );
    }

    #[test]
    fn forward_adapter_clone_preserves_state() {
        let stopper = Stopper::new();
        let stopper_c = stopper.clone();
        let check = StopCheck::from_flag(move || stopper_c.should_stop());

        let clone_a = check.clone();
        let clone_b = check.clone();
        let clone_of_clone = clone_a.clone();

        assert!(check.check().is_ok());
        assert!(clone_a.check().is_ok());
        assert!(clone_b.check().is_ok());
        assert!(clone_of_clone.check().is_ok());

        stopper.cancel();

        assert!(check.check().is_err());
        assert!(clone_a.check().is_err());
        assert!(clone_b.check().is_err());
        assert_eq!(clone_of_clone.check(), Err(ZReason::Cancelled));
    }

    #[test]
    fn forward_adapter_clones_fan_out_to_threads() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::thread;
        use std::time::Duration;

        let stopper = Stopper::new();
        let stopper_c = stopper.clone();
        let check = StopCheck::from_flag(move || stopper_c.should_stop());

        let observed = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let check_c = check.clone();
                let observed = Arc::clone(&observed);
                thread::spawn(move || {
                    while check_c.check().is_ok() {
                        thread::yield_now();
                    }
                    assert_eq!(check_c.check(), Err(ZReason::Cancelled));
                    observed.fetch_add(1, Ordering::Relaxed);
                })
            })
            .collect();

        thread::sleep(Duration::from_millis(10));
        stopper.cancel();

        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(observed.load(Ordering::Relaxed), 8);
    }

    #[test]
    fn forward_bridge_crosses_threads() {
        use std::sync::Arc;
        use std::thread;

        let stop = Stopper::new();
        let stop_c = stop.clone();
        let check = StopCheck::from_flag(move || stop_c.should_stop());
        let decoder = Arc::new(Decoder::new(check));
        let data = Arc::new(vec![0u8; 1_000_000]);

        let decoder_bg = Arc::clone(&decoder);
        let data_bg = Arc::clone(&data);
        let handle = thread::spawn(move || {
            for _ in 0..10_000 {
                match decoder_bg.decode(&data_bg[..100]) {
                    Ok(_) => continue,
                    Err(DecodeError::Stopped(r)) => return Err(r),
                    Err(e) => panic!("unexpected: {:?}", e),
                }
            }
            Ok(())
        });

        stop.cancel();
        assert_eq!(handle.join().unwrap(), Err(ZReason::Cancelled));
    }
}

// ---------------------------------------------------------------------
// Reverse adapter: a `zerodep` user calls an `enough`-using library.
// ---------------------------------------------------------------------

#[cfg(test)]
mod reverse_adapter {
    use super::zerodep::{StopCheck, StopReason as ZReason};
    use almost_enough::StopReason as EReason;
    use almost_enough::{FnStop, Stop, StopToken, Unstoppable};

    #[allow(unreachable_patterns)]
    fn map_reason(r: ZReason) -> EReason {
        match r {
            ZReason::Cancelled => EReason::Cancelled,
            ZReason::TimedOut => EReason::TimedOut,
            _ => EReason::Cancelled,
        }
    }

    fn enough_decode(data: &[u8], stop: impl Stop) -> Result<Vec<u8>, EReason> {
        let mut out = Vec::with_capacity(data.len());
        for (i, chunk) in data.chunks(64).enumerate() {
            if i % 16 == 0 {
                stop.check()?;
            }
            out.extend_from_slice(chunk);
        }
        Ok(out)
    }

    /// may_stop()-aware bridge: StopCheck → StopToken.
    /// Preserves None ↔ Unstoppable round trip.
    fn to_enough_stop(stop: &StopCheck) -> StopToken {
        if stop.may_stop() {
            let s = stop.clone();
            StopToken::new(FnStop::new(move || s.check().is_err()))
        } else {
            StopToken::new(Unstoppable)
        }
    }

    #[test]
    fn zerodep_user_bridges_stopcheck_to_enough() {
        use core::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let flag = Arc::new(AtomicBool::new(false));
        let my_stop = StopCheck::from_atomic(flag.clone());
        let data = vec![0u8; 10_000];

        let stop_c = my_stop.clone();
        let bridged = FnStop::new(move || stop_c.check().is_err());
        assert!(enough_decode(&data, &bridged).is_ok());

        flag.store(true, Ordering::Relaxed);
        let stop_c = my_stop.clone();
        let bridged = FnStop::new(move || stop_c.check().is_err());
        assert_eq!(
            enough_decode(&data, &bridged).unwrap_err(),
            EReason::Cancelled
        );
    }

    #[test]
    fn reverse_adapter_helper_function() {
        use core::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let flag = Arc::new(AtomicBool::new(false));
        let my_stop = StopCheck::from_atomic(flag.clone());

        let data = vec![0u8; 10_000];
        let token = to_enough_stop(&my_stop);
        assert!(token.may_stop());
        assert!(enough_decode(&data, &token).is_ok());

        flag.store(true, Ordering::Relaxed);
        assert_eq!(
            enough_decode(&data, to_enough_stop(&my_stop)).unwrap_err(),
            EReason::Cancelled
        );
    }

    #[test]
    fn reverse_adapter_none_preserves_may_stop() {
        let none = StopCheck::none();
        assert!(!none.may_stop());

        let token = to_enough_stop(&none);
        assert!(!token.may_stop());
        assert!(token.check().is_ok());
    }

    #[test]
    fn reason_preserving_reverse_adapter() {
        struct ReasonPreserving(StopCheck);
        impl Stop for ReasonPreserving {
            fn check(&self) -> Result<(), EReason> {
                self.0.check().map_err(map_reason)
            }
        }

        let my_stop = StopCheck::new(|| Err(ZReason::TimedOut));
        let bridged = ReasonPreserving(my_stop);
        let data = vec![0u8; 10_000];
        assert_eq!(
            enough_decode(&data, &bridged).unwrap_err(),
            EReason::TimedOut
        );
    }

    #[test]
    fn reverse_adapter_fnstop_is_clone_when_closure_captures_stopcheck() {
        use core::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        fn assert_clone<T: Clone>(_: &T) {}

        let flag = Arc::new(AtomicBool::new(false));
        let my_stop = StopCheck::from_atomic(flag.clone());

        let stop_c = my_stop.clone();
        let bridged = FnStop::new(move || stop_c.check().is_err());

        assert_clone(&bridged);

        let bridged_clone = bridged.clone();
        let bridged_clone2 = bridged.clone();

        assert!(!bridged.should_stop());
        assert!(!bridged_clone.should_stop());
        assert!(!bridged_clone2.should_stop());

        flag.store(true, Ordering::Relaxed);

        assert!(bridged.should_stop());
        assert!(bridged_clone.should_stop());
        assert!(bridged_clone2.should_stop());
    }

    #[test]
    fn reverse_adapter_fnstop_clones_fan_out_to_threads() {
        use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::thread;
        use std::time::Duration;

        let flag = Arc::new(AtomicBool::new(false));
        let my_stop = StopCheck::from_atomic(flag.clone());

        let stop_c = my_stop.clone();
        let bridged = FnStop::new(move || stop_c.check().is_err());

        let observed = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let b = bridged.clone();
                let observed = Arc::clone(&observed);
                thread::spawn(move || {
                    let data = vec![0u8; 100];
                    loop {
                        match enough_decode(&data, &b) {
                            Ok(_) => thread::yield_now(),
                            Err(EReason::Cancelled) => {
                                observed.fetch_add(1, Ordering::Relaxed);
                                return;
                            }
                            Err(other) => panic!("unexpected: {:?}", other),
                        }
                    }
                })
            })
            .collect();

        thread::sleep(Duration::from_millis(10));
        flag.store(true, Ordering::Relaxed);

        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(observed.load(Ordering::Relaxed), 8);
    }

    #[test]
    fn reverse_adapter_clone_chain_preserves_state() {
        use core::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let flag = Arc::new(AtomicBool::new(false));
        let stop = StopCheck::from_atomic(flag.clone());
        let stop_clone = stop.clone();
        let fn_stop = FnStop::new(move || stop_clone.check().is_err());
        let fn_stop_clone = fn_stop.clone();

        assert!(stop.check().is_ok());
        assert!(!fn_stop.should_stop());
        assert!(!fn_stop_clone.should_stop());

        flag.store(true, Ordering::Relaxed);

        assert!(stop.check().is_err());
        assert!(fn_stop.should_stop());
        assert!(fn_stop_clone.should_stop());

        flag.store(false, Ordering::Relaxed);
        assert!(stop.check().is_ok());
        assert!(!fn_stop.should_stop());
        assert!(!fn_stop_clone.should_stop());
    }

    #[test]
    fn none_stopcheck_bridges_to_fnstop() {
        let my_stop = StopCheck::none();
        let bridged = FnStop::new(move || my_stop.check().is_err());
        let data = vec![0u8; 1000];
        assert!(enough_decode(&data, &bridged).is_ok());
    }
}

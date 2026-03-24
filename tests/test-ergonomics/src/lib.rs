//! Ergonomics matrix: which Stop types work with which function signatures?
//!
//! This file is both a test suite and living documentation. Every test
//! demonstrates a type × signature combination that compiles. Combinations
//! that DON'T compile are documented in comments.
//!
//! # Function Signatures
//!
//! | Signature               | Monomorphized | Clonable | `None`-able | Dep |
//! |-------------------------|:---:|:---:|:---:|---------|
//! | `impl Stop`             | yes |  no | no* | enough  |
//! | `impl CloneStop`        | yes | yes | no* | enough  |
//! | `&dyn Stop`             |  no |  no |  no | enough  |
//! | `Option<&dyn Stop>`     |  no |  no | yes | enough  |
//! | `DynStop`               |  no | yes | no  | almost-enough |
//! | `Option<DynStop>`       |  no | yes | yes | almost-enough |
//!
//! *`Unstoppable` works but callers must import it; `None` is simpler.
//!
//! # Stop Types
//!
//! | Type          | Clone | Copy | 'static | Notes |
//! |---------------|:-----:|:----:|:-------:|-------|
//! | Unstoppable   |  yes  | yes  |   yes   | Zero-cost no-op |
//! | StopSource    |   no  |  no  |   yes   | Stack-based, owns AtomicBool |
//! | `StopRef<'a>` |  yes  | yes  |   no    | Borrowed — can't wrap in DynStop |
//! | `FnStop<F>`   |  if F | if F |  if F   | Bridges external systems |
//! | Stopper       |  yes  |  no  |   yes   | Default choice (Arc-based) |
//! | SyncStopper   |  yes  |  no  |   yes   | Acquire/Release ordering |
//! | ChildStopper  |  yes  |  no  |   yes   | Hierarchical cancellation |
//! | DynStop       |  yes  |  no  |   yes   | Cloneable type erasure (Arc) |
//! | BoxedStop     |   no  |  no  |   yes   | Non-clonable type erasure |
//!
//! # What DOESN'T compile
//!
//! | Attempt | Why |
//! |---------|-----|
//! | `DynStop::new(source.as_ref())` | StopRef not 'static |
//! | `BoxedStop::new(source.as_ref())` | StopRef not 'static |
//! | `impl CloneStop` with BoxedStop | BoxedStop: !Clone |
//! | `impl CloneStop` with StopSource | StopSource: !Clone |
//! | `stop.clone()` on `&dyn Stop` | Clone not object-safe |
//! | `thread::spawn` with `&dyn Stop` | not 'static |
//! | `Option<S: Stop>` with `None` | can't infer S |

#![cfg(test)]
#![allow(unused_imports)]

use almost_enough::{
    BoxedStop, ChildStopper, DynStop, FnStop, OrStop, Stop, StopExt, StopRef, StopSource, Stopper,
    SyncStopper, Unstoppable,
};

// ═══════════════════════════════════════════════════════════════════
// Target function signatures
// ═══════════════════════════════════════════════════════════════════

fn accept_impl(stop: impl Stop) -> bool {
    stop.should_stop()
}

fn accept_clone_stop(stop: impl almost_enough::CloneStop) -> bool {
    let s2 = stop.clone();
    stop.should_stop() || s2.should_stop()
}

fn accept_dyn(stop: &dyn Stop) -> bool {
    stop.should_stop()
}

fn accept_option_dyn(stop: Option<&dyn Stop>) -> bool {
    stop.should_stop()
}

fn accept_dynstop(stop: DynStop) -> bool {
    stop.should_stop()
}

fn accept_option_dynstop(stop: Option<DynStop>) -> bool {
    stop.should_stop()
}

// ═══════════════════════════════════════════════════════════════════
// Unstoppable → every signature (Copy + 'static)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn unstoppable_to_impl() {
    assert!(!accept_impl(Unstoppable));
}
#[test]
fn unstoppable_to_impl_clone() {
    assert!(!accept_clone_stop(Unstoppable));
}
#[test]
fn unstoppable_to_dyn() {
    assert!(!accept_dyn(&Unstoppable));
}
#[test]
fn unstoppable_to_option_dyn() {
    assert!(!accept_option_dyn(Some(&Unstoppable)));
}
#[test]
fn unstoppable_to_dynstop() {
    assert!(!accept_dynstop(Unstoppable.into_dyn()));
}
#[test]
fn unstoppable_to_option_dynstop() {
    assert!(!accept_option_dynstop(Some(Unstoppable.into_dyn())));
}

// ═══════════════════════════════════════════════════════════════════
// Stopper → every signature (Clone + 'static)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn stopper_to_impl() {
    assert!(!accept_impl(Stopper::new()));
}
#[test]
fn stopper_to_impl_clone() {
    assert!(!accept_clone_stop(Stopper::new()));
}
#[test]
fn stopper_to_dyn() {
    assert!(!accept_dyn(&Stopper::new()));
}
#[test]
fn stopper_to_option_dyn() {
    let s = Stopper::new();
    assert!(!accept_option_dyn(Some(&s)));
}
#[test]
fn stopper_to_dynstop() {
    assert!(!accept_dynstop(Stopper::new().into_dyn()));
}
#[test]
fn stopper_to_option_dynstop() {
    assert!(!accept_option_dynstop(Some(Stopper::new().into_dyn())));
}

// ═══════════════════════════════════════════════════════════════════
// StopSource → NOT Clone (owns AtomicBool)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn stopsource_to_impl() {
    assert!(!accept_impl(StopSource::new()));
}
// StopSource NOT Clone — can't pass to impl Stop + Clone:
//   accept_clone_stop(StopSource::new()); // ERROR
#[test]
fn stopsource_to_dyn() {
    assert!(!accept_dyn(&StopSource::new()));
}
#[test]
fn stopsource_to_option_dyn() {
    let s = StopSource::new();
    assert!(!accept_option_dyn(Some(&s)));
}
#[test]
fn stopsource_to_dynstop() {
    assert!(!accept_dynstop(DynStop::new(StopSource::new())));
}

// ═══════════════════════════════════════════════════════════════════
// StopRef<'a> → limited (not 'static)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn stopref_to_impl() {
    let source = StopSource::new();
    assert!(!accept_impl(source.as_ref()));
}
// StopRef is Clone+Copy but NOT 'static — can't satisfy CloneStop:
//   accept_clone_stop(source.as_ref()); // ERROR: not 'static
#[test]
fn stopref_to_dyn() {
    let source = StopSource::new();
    let r: StopRef<'_> = source.as_ref();
    assert!(!accept_dyn(&r));
}
#[test]
fn stopref_to_option_dyn() {
    let source = StopSource::new();
    let r: StopRef<'_> = source.as_ref();
    assert!(!accept_option_dyn(Some(&r)));
}
// StopRef CANNOT be wrapped in DynStop/BoxedStop (not 'static):
//   DynStop::new(source.as_ref())   // ERROR: lifetime
//   BoxedStop::new(source.as_ref()) // ERROR: lifetime

// ═══════════════════════════════════════════════════════════════════
// FnStop → depends on closure
// ═══════════════════════════════════════════════════════════════════

#[test]
fn fnstop_to_impl() {
    assert!(!accept_impl(FnStop::new(|| false)));
}
#[test]
fn fnstop_clone_to_impl_clone() {
    assert!(!accept_clone_stop(FnStop::new(|| false)));
}
#[test]
fn fnstop_to_dyn() {
    let f = FnStop::new(|| false);
    assert!(!accept_dyn(&f));
}
#[test]
fn fnstop_noclone_to_dynstop() {
    // Non-Clone closure works — DynStop doesn't require Clone on T
    let shared = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let f = {
        let shared = shared.clone();
        FnStop::new(move || shared.load(std::sync::atomic::Ordering::Relaxed))
    };
    assert!(!accept_dynstop(DynStop::new(f)));
}

// ═══════════════════════════════════════════════════════════════════
// DynStop → Clone + 'static — works everywhere
// ═══════════════════════════════════════════════════════════════════

#[test]
fn dynstop_to_impl() {
    assert!(!accept_impl(Stopper::new().into_dyn()));
}
#[test]
fn dynstop_to_impl_clone() {
    assert!(!accept_clone_stop(Stopper::new().into_dyn()));
}
#[test]
fn dynstop_to_dyn() {
    let s = Stopper::new().into_dyn();
    assert!(!accept_dyn(&s));
}
#[test]
fn dynstop_to_option_dyn() {
    let s = Stopper::new().into_dyn();
    assert!(!accept_option_dyn(Some(&s)));
}
#[test]
fn dynstop_to_dynstop() {
    let s = Stopper::new().into_dyn();
    assert!(!accept_dynstop(s.clone()));
}

// ═══════════════════════════════════════════════════════════════════
// BoxedStop → NOT Clone
// ═══════════════════════════════════════════════════════════════════

#[test]
fn boxedstop_to_impl() {
    assert!(!accept_impl(Stopper::new().into_boxed()));
}
// BoxedStop NOT Clone — can't pass to impl Stop + Clone:
//   accept_clone_stop(Stopper::new().into_boxed()); // ERROR
#[test]
fn boxedstop_to_dyn() {
    let s = Stopper::new().into_boxed();
    assert!(!accept_dyn(&s));
}
#[test]
fn boxedstop_to_option_dyn() {
    let s = Stopper::new().into_boxed();
    assert!(!accept_option_dyn(Some(&s)));
}

// ═══════════════════════════════════════════════════════════════════
// SyncStopper, ChildStopper → Clone + 'static
// ═══════════════════════════════════════════════════════════════════

#[test]
fn syncstopper_to_impl_clone() {
    assert!(!accept_clone_stop(SyncStopper::new()));
}
#[test]
fn syncstopper_to_dynstop() {
    assert!(!accept_dynstop(DynStop::new(SyncStopper::new())));
}
#[test]
fn childstopper_to_impl_clone() {
    assert!(!accept_clone_stop(ChildStopper::new()));
}
#[test]
fn childstopper_to_dynstop() {
    assert!(!accept_dynstop(DynStop::new(ChildStopper::new())));
}

// ═══════════════════════════════════════════════════════════════════
// OrStop → Clone/Copy if halves are
// ═══════════════════════════════════════════════════════════════════

// OrStop<StopRef, StopRef> is Copy but NOT 'static — can't satisfy CloneStop:
//   accept_clone_stop(a.as_ref().or(b.as_ref())); // ERROR: not 'static

// ═══════════════════════════════════════════════════════════════════
// Option<&dyn Stop>: the recommended library API
// ═══════════════════════════════════════════════════════════════════

mod option_api {
    use super::*;

    /// The recommended library function signature
    fn library_fn(data: &[u8], stop: Option<&dyn Stop>) -> usize {
        let _ = stop.check();
        data.len()
    }

    #[test]
    fn none_no_imports() {
        // Callers don't need to import anything for "no cancellation"
        assert_eq!(library_fn(b"hello", None), 5);
    }

    #[test]
    fn some_stopper() {
        let s = Stopper::new();
        assert_eq!(library_fn(b"hello", Some(&s)), 5);
    }

    #[test]
    fn some_stopsource() {
        let s = StopSource::new();
        assert_eq!(library_fn(b"hello", Some(&s)), 5);
    }

    #[test]
    fn some_stopref() {
        let source = StopSource::new();
        let r = source.as_ref();
        assert_eq!(library_fn(b"hello", Some(&r)), 5);
    }

    #[test]
    fn some_dynstop() {
        let s = Stopper::new().into_dyn();
        assert_eq!(library_fn(b"hello", Some(&s)), 5);
    }

    #[test]
    fn some_boxedstop() {
        let s = Stopper::new().into_boxed();
        assert_eq!(library_fn(b"hello", Some(&s)), 5);
    }

    #[test]
    fn some_fnstop() {
        let f = FnStop::new(|| false);
        assert_eq!(library_fn(b"hello", Some(&f)), 5);
    }

    #[test]
    fn some_unstoppable() {
        // Works, but None is simpler
        assert_eq!(library_fn(b"hello", Some(&Unstoppable)), 5);
    }

    #[test]
    fn some_childstopper() {
        let s = ChildStopper::new();
        assert_eq!(library_fn(b"hello", Some(&s)), 5);
    }
}

// ═══════════════════════════════════════════════════════════════════
// Thread cloning: DynStop vs impl Stop + Clone
// ═══════════════════════════════════════════════════════════════════

mod cloning {
    use super::*;

    #[test]
    fn dynstop_clone_to_thread() {
        let stop = Stopper::new().into_dyn();
        let s2 = stop.clone();
        let handle = std::thread::spawn(move || s2.should_stop());
        assert!(!handle.join().unwrap());
    }

    #[test]
    fn impl_clone_to_thread() {
        fn spawn_with<S: Stop + Clone + Send + 'static>(stop: S) -> bool {
            let s2 = stop.clone();
            let handle = std::thread::spawn(move || s2.should_stop());
            handle.join().unwrap()
        }

        assert!(!spawn_with(Stopper::new()));
        assert!(!spawn_with(SyncStopper::new()));
        assert!(!spawn_with(ChildStopper::new()));
        assert!(!spawn_with(Unstoppable));
        assert!(!spawn_with(Stopper::new().into_dyn()));
    }

    // Can't clone through &dyn Stop — Clone isn't object-safe:
    //   fn spawn_dyn(stop: &dyn Stop) {
    //       let s = stop.clone(); // ERROR
    //       std::thread::spawn(move || s.check());
    //   }

    // Can't send &dyn Stop to thread — not 'static:
    //   fn spawn_ref(stop: &dyn Stop) {
    //       std::thread::spawn(move || stop.check()); // ERROR: lifetime
    //   }
}

// ═══════════════════════════════════════════════════════════════════
// What DOESN'T compile (summary)
// ═══════════════════════════════════════════════════════════════════
//
// | Attempt                              | Error                |
// |--------------------------------------|----------------------|
// | DynStop::new(source.as_ref())        | StopRef not 'static  |
// | BoxedStop::new(source.as_ref())      | StopRef not 'static  |
// | accept_clone_stop(boxed_stop)        | BoxedStop: !Clone     |
// | stop.clone() on &dyn Stop            | Clone not object-safe |
// | thread::spawn with &dyn Stop         | not 'static          |
// | accept_option_dynstop(None)          | can't infer type     |
// | fn f<S: Stop>(s: Option<S>) { f(None) } | can't infer S    |

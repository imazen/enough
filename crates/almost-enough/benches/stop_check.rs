use std::hint::black_box;
use std::time::Duration;

use almost_enough::{
    BoxedStop, ChildStopper, DynStop, FnStop, OrStop, Stop, StopExt, StopSource, Stopper,
    SyncStopper, TimeoutExt, Unstoppable, WithTimeout,
};
use criterion::{Criterion, criterion_group, criterion_main};

// ── Group: check (hot path, not cancelled) ──────────────────────────

fn bench_check(c: &mut Criterion) {
    let mut g = c.benchmark_group("check");

    // Unstoppable
    g.bench_function("unstoppable", |b| {
        let stop = Unstoppable;
        b.iter(|| black_box(&stop).check())
    });

    // StopSource (stack AtomicBool, Relaxed)
    g.bench_function("stop_source", |b| {
        let stop = StopSource::new();
        b.iter(|| black_box(&stop).check())
    });

    // StopRef (borrowed AtomicBool, Relaxed)
    g.bench_function("stop_ref", |b| {
        let source = StopSource::new();
        let stop = source.as_ref();
        b.iter(|| black_box(&stop).check())
    });

    // Stopper (Arc<AtomicBool>, Relaxed)
    g.bench_function("stopper", |b| {
        let stop = Stopper::new();
        b.iter(|| black_box(&stop).check())
    });

    // SyncStopper (Arc<AtomicBool>, Acquire)
    g.bench_function("sync_stopper", |b| {
        let stop = SyncStopper::new();
        b.iter(|| black_box(&stop).check())
    });

    // FnStop (closure returning false)
    g.bench_function("fn_stop", |b| {
        let stop = FnStop::new(|| false);
        b.iter(|| black_box(&stop).check())
    });

    // BoxedStop wrapping Unstoppable
    g.bench_function("boxed_unstoppable", |b| {
        let stop = Unstoppable.into_boxed();
        b.iter(|| black_box(&stop).check())
    });

    // BoxedStop wrapping Stopper
    g.bench_function("boxed_stopper", |b| {
        let stop = Stopper::new().into_boxed();
        b.iter(|| black_box(&stop).check())
    });

    // DynStop wrapping Unstoppable
    g.bench_function("dyn_unstoppable", |b| {
        let stop = Unstoppable.into_dyn();
        b.iter(|| black_box(&stop).check())
    });

    // DynStop wrapping Stopper
    g.bench_function("dyn_stopper", |b| {
        let stop = Stopper::new().into_dyn();
        b.iter(|| black_box(&stop).check())
    });

    // WithTimeout<StopRef> (inner check + Instant::now())
    g.bench_function("with_timeout", |b| {
        let source = StopSource::new();
        let stop: WithTimeout<_> = source.as_ref().with_timeout(Duration::from_secs(3600));
        b.iter(|| black_box(&stop).check())
    });

    // ChildStopper::new() (no parent)
    g.bench_function("child_root", |b| {
        let stop = ChildStopper::new();
        b.iter(|| black_box(&stop).check())
    });

    // ChildStopper with 1 parent
    g.bench_function("child_depth_1", |b| {
        let parent = ChildStopper::new();
        let stop = parent.child();
        b.iter(|| black_box(&stop).check())
    });

    // ChildStopper with 3 ancestors
    g.bench_function("child_depth_3", |b| {
        let g0 = ChildStopper::new();
        let g1 = g0.child();
        let g2 = g1.child();
        let stop = g2.child();
        b.iter(|| black_box(&stop).check())
    });

    // OrStop<StopRef, StopRef> (two Relaxed loads)
    g.bench_function("or_stop", |b| {
        let a = StopSource::new();
        let b_src = StopSource::new();
        let stop: OrStop<_, _> = a.as_ref().or(b_src.as_ref());
        b.iter(|| black_box(&stop).check())
    });

    g.finish();
}

// ── Group: dispatch (generic vs dyn vs BoxedStop vs DynStop) ─────────

#[inline(never)]
fn check_generic(stop: &impl Stop) -> Result<(), almost_enough::StopReason> {
    stop.check()
}

#[inline(never)]
fn check_dyn(stop: &dyn Stop) -> Result<(), almost_enough::StopReason> {
    stop.check()
}

#[inline(never)]
fn check_boxed(stop: &BoxedStop) -> Result<(), almost_enough::StopReason> {
    stop.check()
}

#[inline(never)]
fn check_dyn_stop(stop: &DynStop) -> Result<(), almost_enough::StopReason> {
    stop.check()
}

fn bench_dispatch(c: &mut Criterion) {
    let mut g = c.benchmark_group("dispatch");

    // Unstoppable through every path
    g.bench_function("unstoppable_generic", |b| {
        let stop = Unstoppable;
        b.iter(|| check_generic(black_box(&stop)))
    });

    g.bench_function("unstoppable_dyn", |b| {
        let stop = Unstoppable;
        b.iter(|| check_dyn(black_box(&stop)))
    });

    g.bench_function("unstoppable_boxed", |b| {
        let stop = Unstoppable.into_boxed();
        b.iter(|| check_boxed(black_box(&stop)))
    });

    g.bench_function("unstoppable_dynstop", |b| {
        let stop = Unstoppable.into_dyn();
        b.iter(|| check_dyn_stop(black_box(&stop)))
    });

    // Stopper through every path
    g.bench_function("stopper_generic", |b| {
        let stop = Stopper::new();
        b.iter(|| check_generic(black_box(&stop)))
    });

    g.bench_function("stopper_dyn", |b| {
        let stop = Stopper::new();
        b.iter(|| check_dyn(black_box(&stop)))
    });

    g.bench_function("stopper_boxed", |b| {
        let stop = Stopper::new().into_boxed();
        b.iter(|| check_boxed(black_box(&stop)))
    });

    g.bench_function("stopper_dynstop", |b| {
        let stop = Stopper::new().into_dyn();
        b.iter(|| check_dyn_stop(black_box(&stop)))
    });

    // DynStop behind &dyn Stop (double dispatch)
    g.bench_function("stopper_dynstop_as_dyn", |b| {
        let stop = Stopper::new().into_dyn();
        b.iter(|| check_dyn(black_box(&stop) as &dyn Stop))
    });

    // BoxedStop behind &dyn Stop (double dispatch)
    g.bench_function("stopper_boxed_as_dyn", |b| {
        let stop = Stopper::new().into_boxed();
        b.iter(|| check_dyn(black_box(&stop) as &dyn Stop))
    });

    g.finish();
}

// ── Group: may_stop + active_stop optimization patterns ──────────────

#[inline(never)]
fn check_dyn_may_stop(stop: &dyn Stop) -> Result<(), almost_enough::StopReason> {
    let stop = stop.may_stop().then_some(stop);
    stop.check()
}

#[inline(never)]
fn check_boxed_active(stop: &BoxedStop) -> Result<(), almost_enough::StopReason> {
    let stop = stop.active_stop();
    stop.check()
}

#[inline(never)]
fn check_dynstop_active(stop: &DynStop) -> Result<(), almost_enough::StopReason> {
    let stop = stop.active_stop();
    stop.check()
}

fn bench_optimization(c: &mut Criterion) {
    let mut g = c.benchmark_group("optimization");

    // Unstoppable: may_stop pattern should eliminate check
    g.bench_function("unstoppable_dyn_raw", |b| {
        let stop = Unstoppable;
        b.iter(|| check_dyn(black_box(&stop)))
    });

    g.bench_function("unstoppable_dyn_may_stop", |b| {
        let stop = Unstoppable;
        b.iter(|| check_dyn_may_stop(black_box(&stop)))
    });

    // BoxedStop(Unstoppable): active_stop should collapse to None
    g.bench_function("boxed_unstoppable_raw", |b| {
        let stop = Unstoppable.into_boxed();
        b.iter(|| check_boxed(black_box(&stop)))
    });

    g.bench_function("boxed_unstoppable_active", |b| {
        let stop = Unstoppable.into_boxed();
        b.iter(|| check_boxed_active(black_box(&stop)))
    });

    // DynStop(Unstoppable): active_stop should collapse to None
    g.bench_function("dyn_unstoppable_raw", |b| {
        let stop = Unstoppable.into_dyn();
        b.iter(|| check_dyn_stop(black_box(&stop)))
    });

    g.bench_function("dyn_unstoppable_active", |b| {
        let stop = Unstoppable.into_dyn();
        b.iter(|| check_dynstop_active(black_box(&stop)))
    });

    // Stopper: active_stop collapses one dispatch layer
    g.bench_function("boxed_stopper_raw", |b| {
        let stop = Stopper::new().into_boxed();
        b.iter(|| check_boxed(black_box(&stop)))
    });

    g.bench_function("boxed_stopper_active", |b| {
        let stop = Stopper::new().into_boxed();
        b.iter(|| check_boxed_active(black_box(&stop)))
    });

    g.bench_function("dyn_stopper_raw", |b| {
        let stop = Stopper::new().into_dyn();
        b.iter(|| check_dyn_stop(black_box(&stop)))
    });

    g.bench_function("dyn_stopper_active", |b| {
        let stop = Stopper::new().into_dyn();
        b.iter(|| check_dynstop_active(black_box(&stop)))
    });

    g.finish();
}

// ── Group: hot loops (realistic workload with periodic checks) ───────

const HOT_LOOP_ITERS: usize = 10_000;
const CHECK_INTERVAL: usize = 64;

/// Trivial work unit to prevent loop elimination
#[inline(always)]
fn trivial_work(i: usize) -> usize {
    black_box(i.wrapping_mul(2654435761))
}

#[inline(never)]
fn hot_loop_generic(stop: &impl Stop) -> usize {
    let mut acc = 0usize;
    for i in 0..HOT_LOOP_ITERS {
        if i % CHECK_INTERVAL == 0 {
            let _ = stop.check();
        }
        acc = acc.wrapping_add(trivial_work(i));
    }
    acc
}

#[inline(never)]
fn hot_loop_dyn(stop: &dyn Stop) -> usize {
    let mut acc = 0usize;
    for i in 0..HOT_LOOP_ITERS {
        if i % CHECK_INTERVAL == 0 {
            let _ = stop.check();
        }
        acc = acc.wrapping_add(trivial_work(i));
    }
    acc
}

#[inline(never)]
fn hot_loop_dyn_may_stop(stop: &dyn Stop) -> usize {
    let stop = stop.may_stop().then_some(stop);
    let mut acc = 0usize;
    for i in 0..HOT_LOOP_ITERS {
        if i % CHECK_INTERVAL == 0 {
            let _ = stop.check();
        }
        acc = acc.wrapping_add(trivial_work(i));
    }
    acc
}

#[inline(never)]
fn hot_loop_boxed_active(stop: &BoxedStop) -> usize {
    let stop = stop.active_stop();
    let mut acc = 0usize;
    for i in 0..HOT_LOOP_ITERS {
        if i % CHECK_INTERVAL == 0 {
            let _ = stop.check();
        }
        acc = acc.wrapping_add(trivial_work(i));
    }
    acc
}

#[inline(never)]
fn hot_loop_dynstop_active(stop: &DynStop) -> usize {
    let stop = stop.active_stop();
    let mut acc = 0usize;
    for i in 0..HOT_LOOP_ITERS {
        if i % CHECK_INTERVAL == 0 {
            let _ = stop.check();
        }
        acc = acc.wrapping_add(trivial_work(i));
    }
    acc
}

fn bench_hot_loop(c: &mut Criterion) {
    let mut g = c.benchmark_group("hot_loop");

    // Unstoppable through various dispatch paths
    g.bench_function("unstoppable_generic", |b| {
        b.iter(|| hot_loop_generic(&Unstoppable))
    });

    g.bench_function("unstoppable_dyn", |b| {
        let stop = Unstoppable;
        b.iter(|| hot_loop_dyn(&stop))
    });

    g.bench_function("unstoppable_dyn_may_stop", |b| {
        let stop = Unstoppable;
        b.iter(|| hot_loop_dyn_may_stop(&stop))
    });

    g.bench_function("unstoppable_boxed_active", |b| {
        let stop = Unstoppable.into_boxed();
        b.iter(|| hot_loop_boxed_active(&stop))
    });

    g.bench_function("unstoppable_dynstop_active", |b| {
        let stop = Unstoppable.into_dyn();
        b.iter(|| hot_loop_dynstop_active(&stop))
    });

    // Stopper through various dispatch paths
    g.bench_function("stopper_generic", |b| {
        let stop = Stopper::new();
        b.iter(|| hot_loop_generic(&stop))
    });

    g.bench_function("stopper_dyn", |b| {
        let stop = Stopper::new();
        b.iter(|| hot_loop_dyn(&stop))
    });

    g.bench_function("stopper_dyn_may_stop", |b| {
        let stop = Stopper::new();
        b.iter(|| hot_loop_dyn_may_stop(&stop))
    });

    g.bench_function("stopper_boxed", |b| {
        let stop = Stopper::new().into_boxed();
        b.iter(|| hot_loop_boxed_active(&stop))
    });

    g.bench_function("stopper_dynstop", |b| {
        let stop = Stopper::new().into_dyn();
        b.iter(|| hot_loop_dynstop_active(&stop))
    });

    g.finish();
}

// ── Group: check_cancelled (error return path) ──────────────────────

fn bench_check_cancelled(c: &mut Criterion) {
    let mut g = c.benchmark_group("check_cancelled");

    g.bench_function("stopper", |b| {
        let stop = Stopper::cancelled();
        b.iter(|| black_box(&stop).check())
    });

    g.bench_function("sync_stopper", |b| {
        let stop = SyncStopper::cancelled();
        b.iter(|| black_box(&stop).check())
    });

    g.finish();
}

criterion_group!(
    benches,
    bench_check,
    bench_dispatch,
    bench_optimization,
    bench_hot_loop,
    bench_check_cancelled
);
criterion_main!(benches);

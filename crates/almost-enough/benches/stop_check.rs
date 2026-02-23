use std::hint::black_box;
use std::time::Duration;

use almost_enough::{
    ChildStopper, FnStop, OrStop, Stop, StopExt, StopSource, Stopper, SyncStopper, TimeoutExt,
    Unstoppable, WithTimeout,
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

// ── Group: dispatch (generic vs dynamic) ────────────────────────────

#[inline(never)]
fn check_generic(stop: &impl Stop) -> Result<(), almost_enough::StopReason> {
    stop.check()
}

#[inline(never)]
fn check_dyn(stop: &dyn Stop) -> Result<(), almost_enough::StopReason> {
    stop.check()
}

fn bench_dispatch(c: &mut Criterion) {
    let mut g = c.benchmark_group("dispatch");

    g.bench_function("unstoppable_generic", |b| {
        let stop = Unstoppable;
        b.iter(|| check_generic(black_box(&stop)))
    });

    g.bench_function("unstoppable_dyn", |b| {
        let stop = Unstoppable;
        b.iter(|| check_dyn(black_box(&stop)))
    });

    g.bench_function("stopper_generic", |b| {
        let stop = Stopper::new();
        b.iter(|| check_generic(black_box(&stop)))
    });

    g.bench_function("stopper_dyn", |b| {
        let stop = Stopper::new();
        b.iter(|| check_dyn(black_box(&stop)))
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

criterion_group!(benches, bench_check, bench_dispatch, bench_check_cancelled);
criterion_main!(benches);

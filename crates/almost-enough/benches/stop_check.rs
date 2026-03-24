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

    g.bench_function("unstoppable", |b| {
        let stop = Unstoppable;
        b.iter(|| black_box(&stop).check())
    });

    g.bench_function("stop_source", |b| {
        let stop = StopSource::new();
        b.iter(|| black_box(&stop).check())
    });

    g.bench_function("stop_ref", |b| {
        let source = StopSource::new();
        let stop = source.as_ref();
        b.iter(|| black_box(&stop).check())
    });

    g.bench_function("stopper", |b| {
        let stop = Stopper::new();
        b.iter(|| black_box(&stop).check())
    });

    g.bench_function("sync_stopper", |b| {
        let stop = SyncStopper::new();
        b.iter(|| black_box(&stop).check())
    });

    g.bench_function("fn_stop", |b| {
        let stop = FnStop::new(|| false);
        b.iter(|| black_box(&stop).check())
    });

    g.bench_function("boxed_stopper", |b| {
        let stop = Stopper::new().into_boxed();
        b.iter(|| black_box(&stop).check())
    });

    g.bench_function("dyn_stopper", |b| {
        let stop = Stopper::new().into_token();
        b.iter(|| black_box(&stop).check())
    });

    g.bench_function("or_stop", |b| {
        let a = StopSource::new();
        let b_src = StopSource::new();
        let stop: OrStop<_, _> = a.as_ref().or(b_src.as_ref());
        b.iter(|| black_box(&stop).check())
    });

    g.bench_function("child_depth_1", |b| {
        let parent = ChildStopper::new();
        let stop = parent.child();
        b.iter(|| black_box(&stop).check())
    });

    g.bench_function("child_depth_3", |b| {
        let g0 = ChildStopper::new();
        let g1 = g0.child();
        let g2 = g1.child();
        let stop = g2.child();
        b.iter(|| black_box(&stop).check())
    });

    g.bench_function("with_timeout", |b| {
        let source = StopSource::new();
        let stop: WithTimeout<_> = source.as_ref().with_timeout(Duration::from_secs(3600));
        b.iter(|| black_box(&stop).check())
    });

    g.finish();
}

// ── Group: dispatch (generic vs dyn vs StopToken) ─────────────────────

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

    g.bench_function("stopper_generic", |b| {
        let stop = Stopper::new();
        b.iter(|| check_generic(black_box(&stop)))
    });

    g.bench_function("stopper_dyn", |b| {
        let stop = Stopper::new();
        b.iter(|| check_dyn(black_box(&stop)))
    });

    g.bench_function("stopper_dynstop", |b| {
        let stop = Stopper::new().into_token();
        b.iter(|| black_box(&stop).check())
    });

    g.bench_function("stopper_dynstop_as_dyn", |b| {
        let stop = Stopper::new().into_token();
        b.iter(|| check_dyn(black_box(&stop) as &dyn Stop))
    });

    g.bench_function("unstoppable_generic", |b| {
        let stop = Unstoppable;
        b.iter(|| check_generic(black_box(&stop)))
    });

    g.bench_function("unstoppable_dyn", |b| {
        let stop = Unstoppable;
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

// ── Group: hot_loop (impl Stop vs dyn Stop vs StopToken) ──────────────
// Proves that impl Stop inlining is negligible vs dyn dispatch in
// realistic loops with work between checks.

const HOT_LOOP_ITERS: usize = 10_000;
const CHECK_INTERVAL: usize = 64;

#[inline(always)]
fn trivial_work(i: usize) -> usize {
    black_box(i.wrapping_mul(2654435761))
}

macro_rules! hot_loop {
    ($stop:expr) => {{
        let stop = &$stop;
        let mut acc = 0usize;
        for i in 0..HOT_LOOP_ITERS {
            if i % CHECK_INTERVAL == 0 {
                let _ = stop.check();
            }
            acc = acc.wrapping_add(trivial_work(i));
        }
        black_box(acc)
    }};
}

fn bench_hot_loop(c: &mut Criterion) {
    let mut g = c.benchmark_group("hot_loop_stopper");

    // impl Stop — fully inlined, direct call
    g.bench_function("impl_stop", |b| {
        let stop = Stopper::new();
        b.iter(|| hot_loop!(stop))
    });

    // &dyn Stop + may_stop — one-time Option conversion
    g.bench_function("dyn_may_stop", |b| {
        let stop = Stopper::new();
        let stop: &dyn Stop = &stop;
        let stop = stop.may_stop().then_some(stop);
        b.iter(|| hot_loop!(stop))
    });

    // StopToken — automatic Option optimization
    g.bench_function("dynstop", |b| {
        let stop = Stopper::new().into_token();
        b.iter(|| hot_loop!(stop))
    });

    // &dyn Stop raw — no optimization
    g.bench_function("dyn_raw", |b| {
        let stop = Stopper::new();
        let stop: &dyn Stop = &stop;
        b.iter(|| hot_loop!(stop))
    });

    g.finish();

    let mut g = c.benchmark_group("hot_loop_unstoppable");

    g.bench_function("impl_stop", |b| {
        let stop = Unstoppable;
        b.iter(|| hot_loop!(stop))
    });

    g.bench_function("dyn_may_stop", |b| {
        let stop = Unstoppable;
        let stop: &dyn Stop = &stop;
        let stop = stop.may_stop().then_some(stop);
        b.iter(|| hot_loop!(stop))
    });

    g.bench_function("dynstop", |b| {
        let stop = Unstoppable.into_token();
        b.iter(|| hot_loop!(stop))
    });

    // Option<&dyn Stop> = None, constructed directly (not via may_stop)
    g.bench_function("option_none_direct", |b| {
        let stop: Option<&dyn Stop> = None;
        b.iter(|| hot_loop!(stop))
    });

    g.bench_function("dyn_raw", |b| {
        let stop = Unstoppable;
        let stop: &dyn Stop = &stop;
        b.iter(|| hot_loop!(stop))
    });

    g.finish();
}

criterion_group!(
    benches,
    bench_check,
    bench_dispatch,
    bench_check_cancelled,
    bench_hot_loop
);
criterion_main!(benches);

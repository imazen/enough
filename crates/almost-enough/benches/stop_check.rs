//! Per-type cancellation check benchmarks.
//!
//! Measures the raw check() cost for every Stop implementation and
//! dispatch path. Complements stop_check_zen (which focuses on
//! codec-realistic, layout-immune comparisons) with isolated per-type
//! measurements including types not covered there (StopRef, ChildStopper,
//! OrStop, WithTimeout).
//!
//! Run with: cargo bench --bench stop_check

use std::hint::black_box;
use std::time::Duration;

use almost_enough::{
    ChildStopper, FnStop, OrStop, Stop, StopExt, StopReason, StopSource, Stopper, SyncStopper,
    TimeoutExt, Unstoppable, WithTimeout,
};

const HOT_LOOP_ITERS: usize = 10_000;
const CHECK_INTERVAL: usize = 64;

#[inline(always)]
fn trivial_work(i: usize) -> usize {
    black_box(i.wrapping_mul(2654435761))
}

#[inline(never)]
fn check_generic(stop: &impl Stop) -> Result<(), StopReason> {
    stop.check()
}

#[inline(never)]
fn check_dyn(stop: &dyn Stop) -> Result<(), StopReason> {
    stop.check()
}

fn main() {
    let result = zenbench::run(|suite| {
        // ═══════════════════════════════════════════════════════════
        // 1. Per-type check() cost (hot path, not cancelled)
        // ═══════════════════════════════════════════════════════════

        suite.compare("check", |group| {
            group.config().sort_by_speed(true).cache_firewall(false);
            group.baseline("unstoppable");

            group.bench("unstoppable", |b| {
                let stop = Unstoppable;
                b.iter(|| black_box(&stop).check())
            });

            group.bench("stop_source", |b| {
                let stop = StopSource::new();
                b.iter(|| black_box(&stop).check())
            });

            group.bench("stop_ref", |b| {
                let source = StopSource::new();
                let stop = source.as_ref();
                b.iter(|| black_box(&stop).check())
            });

            group.bench("stopper", |b| {
                let stop = Stopper::new();
                b.iter(|| black_box(&stop).check())
            });

            group.bench("sync_stopper", |b| {
                let stop = SyncStopper::new();
                b.iter(|| black_box(&stop).check())
            });

            group.bench("fn_stop", |b| {
                let stop = FnStop::new(|| false);
                b.iter(|| black_box(&stop).check())
            });

            group.bench("boxed_stopper", |b| {
                let stop = Stopper::new().into_boxed();
                b.iter(|| black_box(&stop).check())
            });

            group.bench("dyn_stopper", |b| {
                let stop = Stopper::new().into_token();
                b.iter(|| black_box(&stop).check())
            });

            group.bench("or_stop", |b| {
                let a = StopSource::new();
                let b_src = StopSource::new();
                let stop: OrStop<_, _> = a.as_ref().or(b_src.as_ref());
                b.iter(|| black_box(&stop).check())
            });

            group.bench("child_depth_1", |b| {
                let parent = ChildStopper::new();
                let stop = parent.child();
                b.iter(|| black_box(&stop).check())
            });

            group.bench("child_depth_3", |b| {
                let g0 = ChildStopper::new();
                let g1 = g0.child();
                let g2 = g1.child();
                let stop = g2.child();
                b.iter(|| black_box(&stop).check())
            });

            group.bench("with_timeout", |b| {
                let source = StopSource::new();
                let stop: WithTimeout<_> = source.as_ref().with_timeout(Duration::from_secs(3600));
                b.iter(|| black_box(&stop).check())
            });
        });

        // ═══════════════════════════════════════════════════════════
        // 2. Dispatch: generic vs dyn vs StopToken
        // ═══════════════════════════════════════════════════════════

        suite.compare("dispatch", |group| {
            group.config().sort_by_speed(true).cache_firewall(false);
            group.baseline("stopper_generic");

            group.bench("stopper_generic", |b| {
                let stop = Stopper::new();
                b.iter(|| check_generic(black_box(&stop)))
            });

            group.bench("stopper_dyn", |b| {
                let stop = Stopper::new();
                b.iter(|| check_dyn(black_box(&stop)))
            });

            group.bench("stopper_dynstop", |b| {
                let stop = Stopper::new().into_token();
                b.iter(|| black_box(&stop).check())
            });

            group.bench("stopper_dynstop_as_dyn", |b| {
                let stop = Stopper::new().into_token();
                b.iter(|| check_dyn(black_box(&stop) as &dyn Stop))
            });

            group.bench("unstoppable_generic", |b| {
                let stop = Unstoppable;
                b.iter(|| check_generic(black_box(&stop)))
            });

            group.bench("unstoppable_dyn", |b| {
                let stop = Unstoppable;
                b.iter(|| check_dyn(black_box(&stop)))
            });
        });

        // ═══════════════════════════════════════════════════════════
        // 3. Cancelled path (error return)
        // ═══════════════════════════════════════════════════════════

        suite.compare("check_cancelled", |group| {
            group.config().sort_by_speed(true).cache_firewall(false);
            group.baseline("stopper");

            group.bench("stopper", |b| {
                let stop = Stopper::cancelled();
                b.iter(|| black_box(&stop).check())
            });

            group.bench("sync_stopper", |b| {
                let stop = SyncStopper::cancelled();
                b.iter(|| black_box(&stop).check())
            });
        });

        // ═══════════════════════════════════════════════════════════
        // 4. Hot loop: impl Stop vs dyn Stop vs StopToken
        //
        // Proves that impl Stop inlining is negligible vs dyn dispatch
        // in realistic loops with work between checks.
        // ═══════════════════════════════════════════════════════════

        suite.compare("hot_loop_stopper", |group| {
            group.config().sort_by_speed(true).cache_firewall(false);
            group.baseline("impl_stop");
            group.throughput(zenbench::Throughput::Elements(HOT_LOOP_ITERS as u64));

            group.bench("impl_stop", |b| {
                let stop = Stopper::new();
                b.iter(|| {
                    let mut acc = 0usize;
                    for i in 0..HOT_LOOP_ITERS {
                        if i % CHECK_INTERVAL == 0 {
                            let _ = stop.check();
                        }
                        acc = acc.wrapping_add(trivial_work(i));
                    }
                    black_box(acc)
                })
            });

            group.bench("stoptoken", |b| {
                let stop = Stopper::new().into_token();
                b.iter(|| {
                    let mut acc = 0usize;
                    for i in 0..HOT_LOOP_ITERS {
                        if i % CHECK_INTERVAL == 0 {
                            let _ = stop.check();
                        }
                        acc = acc.wrapping_add(trivial_work(i));
                    }
                    black_box(acc)
                })
            });

            group.bench("dyn_may_stop", |b| {
                let stop = Stopper::new();
                let stop: &dyn Stop = &stop;
                let stop = stop.may_stop().then_some(stop);
                b.iter(|| {
                    let mut acc = 0usize;
                    for i in 0..HOT_LOOP_ITERS {
                        if i % CHECK_INTERVAL == 0 {
                            let _ = stop.check();
                        }
                        acc = acc.wrapping_add(trivial_work(i));
                    }
                    black_box(acc)
                })
            });

            group.bench("dyn_raw", |b| {
                let stop = Stopper::new();
                let stop: &dyn Stop = &stop;
                b.iter(|| {
                    let mut acc = 0usize;
                    for i in 0..HOT_LOOP_ITERS {
                        if i % CHECK_INTERVAL == 0 {
                            let _ = stop.check();
                        }
                        acc = acc.wrapping_add(trivial_work(i));
                    }
                    black_box(acc)
                })
            });
        });

        suite.compare("hot_loop_unstoppable", |group| {
            group.config().sort_by_speed(true).cache_firewall(false);
            group.baseline("impl_stop");
            group.throughput(zenbench::Throughput::Elements(HOT_LOOP_ITERS as u64));

            group.bench("impl_stop", |b| {
                let stop = Unstoppable;
                b.iter(|| {
                    let mut acc = 0usize;
                    for i in 0..HOT_LOOP_ITERS {
                        if i % CHECK_INTERVAL == 0 {
                            let _ = stop.check();
                        }
                        acc = acc.wrapping_add(trivial_work(i));
                    }
                    black_box(acc)
                })
            });

            group.bench("stoptoken", |b| {
                let stop = Unstoppable.into_token();
                b.iter(|| {
                    let mut acc = 0usize;
                    for i in 0..HOT_LOOP_ITERS {
                        if i % CHECK_INTERVAL == 0 {
                            let _ = stop.check();
                        }
                        acc = acc.wrapping_add(trivial_work(i));
                    }
                    black_box(acc)
                })
            });

            group.bench("dyn_may_stop", |b| {
                let stop = Unstoppable;
                let stop: &dyn Stop = &stop;
                let stop = stop.may_stop().then_some(stop);
                b.iter(|| {
                    let mut acc = 0usize;
                    for i in 0..HOT_LOOP_ITERS {
                        if i % CHECK_INTERVAL == 0 {
                            let _ = stop.check();
                        }
                        acc = acc.wrapping_add(trivial_work(i));
                    }
                    black_box(acc)
                })
            });

            group.bench("dyn_raw", |b| {
                let stop = Unstoppable;
                let stop: &dyn Stop = &stop;
                b.iter(|| {
                    let mut acc = 0usize;
                    for i in 0..HOT_LOOP_ITERS {
                        if i % CHECK_INTERVAL == 0 {
                            let _ = stop.check();
                        }
                        acc = acc.wrapping_add(trivial_work(i));
                    }
                    black_box(acc)
                })
            });
        });
    });

    if let Err(e) = result.save("stop_check_results.json") {
        eprintln!("Failed to save results: {e}");
    }
}

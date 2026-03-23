//! Interleaved microbenchmarks for Stop dispatch patterns using zenbench.
//!
//! Unlike criterion (which runs each benchmark sequentially), zenbench
//! interleaves samples from all benchmarks in a comparison group, ensuring
//! each variant is measured under identical system conditions.
//!
//! Run with: cargo bench --bench stop_check_zen

use std::time::Duration;

use almost_enough::{
    BoxedStop, ChildStopper, DynStop, FnStop, OrStop, Stop, StopExt, StopSource, Stopper,
    SyncStopper, TimeoutExt, Unstoppable,
};

const HOT_LOOP_ITERS: usize = 10_000;
const CHECK_INTERVAL: usize = 64;

/// Trivial work unit to prevent loop elimination
#[inline(always)]
fn trivial_work(i: usize) -> usize {
    zenbench::black_box(i.wrapping_mul(2654435761))
}

// ── Helpers: various fn signatures ──────────────────────────────────

#[inline(never)]
fn check_generic(stop: &impl Stop) -> Result<(), almost_enough::StopReason> {
    stop.check()
}

#[inline(never)]
fn check_dyn(stop: &dyn Stop) -> Result<(), almost_enough::StopReason> {
    stop.check()
}

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

// ── Helpers: hot loop variants ──────────────────────────────────────

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

/// Run check() 100 times to lift measurement above timer resolution.
/// Per-call time = measured / 100.
#[inline(never)]
fn check_100(stop: &dyn Stop) {
    for _ in 0..100 {
        let _ = zenbench::black_box(stop).check();
    }
}

fn main() {
    let result = zenbench::run(|suite| {
        // ── Batched check (100x inner loop) ─────────────────────────
        // Lifts sub-ns operations above timer resolution for validation.
        // Reported times are for 100 calls. Divide by 100 for per-call cost.

        suite.compare("check_100x", |group| {
            group.config().rounds(200).cache_firewall(false).sort_by_speed(true);
            group.throughput(zenbench::Throughput::Elements(100));
            group.throughput_unit("checks");
            group.baseline("impl Stop (Unstoppable)");

            // -- Generic (compiler sees concrete type) --

            group.bench("impl Stop (Unstoppable)", |b| {
                let stop = Unstoppable;
                b.iter(|| {
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&stop).check();
                    }
                })
            });

            group.bench("impl Stop (Stopper)", |b| {
                let stop = Stopper::new();
                b.iter(|| {
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&stop).check();
                    }
                })
            });

            // -- Single dyn dispatch --

            group.bench("&dyn Stop (Unstoppable)", |b| {
                let stop = Unstoppable;
                b.iter(|| check_100(&stop))
            });

            group.bench("&dyn Stop (Stopper)", |b| {
                let stop = Stopper::new();
                b.iter(|| check_100(&stop))
            });

            // -- &DynStop (concrete type known, one inner dispatch) --

            group.bench("&DynStop (Unstoppable)", |b| {
                let stop = Unstoppable.into_dyn();
                b.iter(|| {
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&stop).check();
                    }
                })
            });

            group.bench("&DynStop (Stopper)", |b| {
                let stop = Stopper::new().into_dyn();
                b.iter(|| {
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&stop).check();
                    }
                })
            });

            // -- DynStop owned (includes Arc::clone per sample) --

            group.bench("DynStop owned (Unstoppable)", |b| {
                let stop = Unstoppable.into_dyn();
                b.iter(|| {
                    let stop = zenbench::black_box(&stop).clone();
                    for _ in 0..100 {
                        let _ = stop.check();
                    }
                })
            });

            group.bench("DynStop owned (Stopper)", |b| {
                let stop = Stopper::new().into_dyn();
                b.iter(|| {
                    let stop = zenbench::black_box(&stop).clone();
                    for _ in 0..100 {
                        let _ = stop.check();
                    }
                })
            });

            // -- DynStop behind &dyn Stop (double dispatch) --

            group.bench("&dyn Stop <- DynStop (Unstoppable)", |b| {
                let stop = Unstoppable.into_dyn();
                b.iter(|| check_100(&stop as &dyn Stop))
            });

            group.bench("&dyn Stop <- DynStop (Stopper)", |b| {
                let stop = Stopper::new().into_dyn();
                b.iter(|| check_100(&stop as &dyn Stop))
            });

            // -- BoxedStop behind &dyn Stop (double dispatch) --

            group.bench("&dyn Stop <- BoxedStop (Unstoppable)", |b| {
                let stop = Unstoppable.into_boxed();
                b.iter(|| check_100(&stop as &dyn Stop))
            });

            group.bench("&dyn Stop <- BoxedStop (Stopper)", |b| {
                let stop = Stopper::new().into_boxed();
                b.iter(|| check_100(&stop as &dyn Stop))
            });

            // -- DynStop.active_stop() (collapses to inner) --

            group.bench("DynStop.active_stop (Unstoppable)", |b| {
                let stop = Unstoppable.into_dyn();
                b.iter(|| {
                    let active = stop.active_stop();
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&active).check();
                    }
                })
            });

            group.bench("DynStop.active_stop (Stopper)", |b| {
                let stop = Stopper::new().into_dyn();
                b.iter(|| {
                    let active = stop.active_stop();
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&active).check();
                    }
                })
            });

            // -- Option patterns (the may_stop() payoff) --

            group.bench("Option<&dyn Stop> = None", |b| {
                let stop: Option<&dyn Stop> = None;
                b.iter(|| {
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&stop).check();
                    }
                })
            });

            group.bench("Option<&dyn Stop> = Some(Stopper)", |b| {
                let stopper = Stopper::new();
                let stop: Option<&dyn Stop> = Some(&stopper);
                b.iter(|| {
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&stop).check();
                    }
                })
            });
        });

        // ── Single check: all Stop types ────────────────────────────

        suite.compare("check_types", |group| {
            group.config().rounds(200).cache_firewall(false).sort_by_speed(true);

            group.bench("unstoppable", |b| {
                let stop = Unstoppable;
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("stop_source", |b| {
                let stop = StopSource::new();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("stop_ref", |b| {
                let source = StopSource::new();
                let stop = source.as_ref();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("stopper", |b| {
                let stop = Stopper::new();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("sync_stopper", |b| {
                let stop = SyncStopper::new();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("fn_stop", |b| {
                let stop = FnStop::new(|| false);
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("boxed_unstoppable", |b| {
                let stop = Unstoppable.into_boxed();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("boxed_stopper", |b| {
                let stop = Stopper::new().into_boxed();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("dyn_unstoppable", |b| {
                let stop = Unstoppable.into_dyn();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("dyn_stopper", |b| {
                let stop = Stopper::new().into_dyn();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("with_timeout", |b| {
                let source = StopSource::new();
                let stop = source.as_ref().with_timeout(Duration::from_secs(3600));
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("child_root", |b| {
                let stop = ChildStopper::new();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("child_depth_1", |b| {
                let parent = ChildStopper::new();
                let stop = parent.child();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("child_depth_3", |b| {
                let g0 = ChildStopper::new();
                let g1 = g0.child();
                let g2 = g1.child();
                let stop = g2.child();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("or_stop", |b| {
                let a = StopSource::new();
                let b_src = StopSource::new();
                let stop: OrStop<_, _> = a.as_ref().or(b_src.as_ref());
                b.iter(|| zenbench::black_box(&stop).check())
            });
        });

        // ── Dispatch: Stopper through every fn signature ────────────

        suite.compare("dispatch_stopper", |group| {
            group.config().rounds(200).cache_firewall(false).sort_by_speed(true);
            group.baseline("generic");

            group.bench("generic", |b| {
                let stop = Stopper::new();
                b.iter(|| check_generic(zenbench::black_box(&stop)))
            });

            group.bench("dyn", |b| {
                let stop = Stopper::new();
                b.iter(|| check_dyn(zenbench::black_box(&stop)))
            });

            group.bench("boxed", |b| {
                let stop = Stopper::new().into_boxed();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("dynstop", |b| {
                let stop = Stopper::new().into_dyn();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("dynstop_as_dyn", |b| {
                let stop = Stopper::new().into_dyn();
                b.iter(|| check_dyn(zenbench::black_box(&stop) as &dyn Stop))
            });

            group.bench("boxed_as_dyn", |b| {
                let stop = Stopper::new().into_boxed();
                b.iter(|| check_dyn(zenbench::black_box(&stop) as &dyn Stop))
            });
        });

        // ── Dispatch: Unstoppable through every fn signature ────────

        suite.compare("dispatch_unstoppable", |group| {
            group.config().rounds(200).cache_firewall(false).sort_by_speed(true);
            group.baseline("generic");

            group.bench("generic", |b| {
                let stop = Unstoppable;
                b.iter(|| check_generic(zenbench::black_box(&stop)))
            });

            group.bench("dyn", |b| {
                let stop = Unstoppable;
                b.iter(|| check_dyn(zenbench::black_box(&stop)))
            });

            group.bench("dyn_may_stop", |b| {
                let stop = Unstoppable;
                b.iter(|| check_dyn_may_stop(zenbench::black_box(&stop)))
            });

            group.bench("boxed", |b| {
                let stop = Unstoppable.into_boxed();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("boxed_active", |b| {
                let stop = Unstoppable.into_boxed();
                b.iter(|| check_boxed_active(zenbench::black_box(&stop)))
            });

            group.bench("dynstop", |b| {
                let stop = Unstoppable.into_dyn();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("dynstop_active", |b| {
                let stop = Unstoppable.into_dyn();
                b.iter(|| check_dynstop_active(zenbench::black_box(&stop)))
            });
        });

        // ── Optimization: raw vs may_stop vs active_stop ────────────

        suite.compare("optimize_unstoppable", |group| {
            group.config().rounds(200).cache_firewall(false).sort_by_speed(true);
            group.baseline("dyn_raw");

            group.bench("dyn_raw", |b| {
                let stop = Unstoppable;
                b.iter(|| check_dyn(zenbench::black_box(&stop)))
            });

            group.bench("dyn_may_stop", |b| {
                let stop = Unstoppable;
                b.iter(|| check_dyn_may_stop(zenbench::black_box(&stop)))
            });

            group.bench("boxed_raw", |b| {
                let stop = Unstoppable.into_boxed();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("boxed_active", |b| {
                let stop = Unstoppable.into_boxed();
                b.iter(|| check_boxed_active(zenbench::black_box(&stop)))
            });

            group.bench("dynstop_raw", |b| {
                let stop = Unstoppable.into_dyn();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("dynstop_active", |b| {
                let stop = Unstoppable.into_dyn();
                b.iter(|| check_dynstop_active(zenbench::black_box(&stop)))
            });
        });

        suite.compare("optimize_stopper", |group| {
            group.config().rounds(200).cache_firewall(false).sort_by_speed(true);
            group.baseline("dyn_raw");

            group.bench("dyn_raw", |b| {
                let stop = Stopper::new();
                b.iter(|| check_dyn(zenbench::black_box(&stop)))
            });

            group.bench("dyn_may_stop", |b| {
                let stop = Stopper::new();
                b.iter(|| check_dyn_may_stop(zenbench::black_box(&stop)))
            });

            group.bench("boxed_raw", |b| {
                let stop = Stopper::new().into_boxed();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("boxed_active", |b| {
                let stop = Stopper::new().into_boxed();
                b.iter(|| check_boxed_active(zenbench::black_box(&stop)))
            });

            group.bench("dynstop_raw", |b| {
                let stop = Stopper::new().into_dyn();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("dynstop_active", |b| {
                let stop = Stopper::new().into_dyn();
                b.iter(|| check_dynstop_active(zenbench::black_box(&stop)))
            });
        });

        // ── Hot loops: 10k iterations, check every 64 ───────────────

        suite.compare("hot_loop_unstoppable", |group| {
            group.config().rounds(100).cache_firewall(false).sort_by_speed(true);
            group.baseline("generic");

            group.bench("generic", |b| b.iter(|| hot_loop_generic(&Unstoppable)));

            group.bench("dyn", |b| {
                let stop = Unstoppable;
                b.iter(|| hot_loop_dyn(&stop))
            });

            group.bench("dyn_may_stop", |b| {
                let stop = Unstoppable;
                b.iter(|| hot_loop_dyn_may_stop(&stop))
            });

            group.bench("boxed_active", |b| {
                let stop = Unstoppable.into_boxed();
                b.iter(|| hot_loop_boxed_active(&stop))
            });

            group.bench("dynstop_active", |b| {
                let stop = Unstoppable.into_dyn();
                b.iter(|| hot_loop_dynstop_active(&stop))
            });
        });

        suite.compare("hot_loop_stopper", |group| {
            group.config().rounds(100).cache_firewall(false).sort_by_speed(true);
            group.baseline("generic");

            group.bench("generic", |b| {
                let stop = Stopper::new();
                b.iter(|| hot_loop_generic(&stop))
            });

            group.bench("dyn", |b| {
                let stop = Stopper::new();
                b.iter(|| hot_loop_dyn(&stop))
            });

            group.bench("dyn_may_stop", |b| {
                let stop = Stopper::new();
                b.iter(|| hot_loop_dyn_may_stop(&stop))
            });

            group.bench("boxed_active", |b| {
                let stop = Stopper::new().into_boxed();
                b.iter(|| hot_loop_boxed_active(&stop))
            });

            group.bench("dynstop_active", |b| {
                let stop = Stopper::new().into_dyn();
                b.iter(|| hot_loop_dynstop_active(&stop))
            });
        });

        // ── Error path: cancelled ───────────────────────────────────

        suite.compare("check_cancelled", |group| {
            group.config().rounds(200).cache_firewall(false);

            group.bench("stopper", |b| {
                let stop = Stopper::cancelled();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("sync_stopper", |b| {
                let stop = SyncStopper::cancelled();
                b.iter(|| zenbench::black_box(&stop).check())
            });
        });

        // ── Cold cache: same hot loops WITH cache firewall ──────────
        // Shows the cost when stop token data is evicted from L1/L2,
        // e.g., after context switches or large working sets.

        suite.compare("cold_cache_stopper", |group| {
            group.config().rounds(100).cache_firewall(true).sort_by_speed(true);
            group.baseline("dyn");

            group.bench("dyn", |b| {
                let stop = Stopper::new();
                b.iter(|| hot_loop_dyn(&stop))
            });

            group.bench("boxed_active", |b| {
                let stop = Stopper::new().into_boxed();
                b.iter(|| hot_loop_boxed_active(&stop))
            });

            group.bench("dynstop_active", |b| {
                let stop = Stopper::new().into_dyn();
                b.iter(|| hot_loop_dynstop_active(&stop))
            });
        });
    });

    if let Err(e) = result.save("stop_check_zen_results.json") {
        eprintln!("Failed to save results: {e}");
    }
}

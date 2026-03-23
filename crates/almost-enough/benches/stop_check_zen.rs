//! Interleaved microbenchmarks for Stop dispatch patterns using zenbench.
//!
//! Run with: cargo bench --bench stop_check_zen

use std::time::Duration;

use almost_enough::{
    BoxedStop, ChildStopper, DynStop, FnStop, OrStop, Stop, StopExt, StopSource, Stopper,
    SyncStopper, TimeoutExt, Unstoppable,
};

const HOT_LOOP_ITERS: usize = 10_000;
const CHECK_INTERVAL: usize = 64;

#[inline(always)]
fn trivial_work(i: usize) -> usize {
    zenbench::black_box(i.wrapping_mul(2654435761))
}

/// Hot loop that checks `stop` every CHECK_INTERVAL iterations.
/// `black_box` is on the accumulator result only — the stop reference
/// is not obscured, so the compiler can optimize check() normally.
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
        zenbench::black_box(acc)
    }};
}

#[inline(never)]
fn check_100(stop: &dyn Stop) {
    for _ in 0..100 {
        let _ = zenbench::black_box(stop).check();
    }
}

fn main() {
    let result = zenbench::run(|suite| {
        // ═══════════════════════════════════════════════════════════
        // Per-call cost: 100x inner loop for timer accuracy.
        // Reported times are for 100 calls. Throughput shows per-call rate.
        // ═══════════════════════════════════════════════════════════

        suite.compare("per_call_cost", |group| {
            group.config().rounds(200).cache_firewall(false).sort_by_speed(true);
            group.throughput(zenbench::Throughput::Elements(100));
            group.throughput_unit("checks");
            group.baseline("impl Stop (Unstoppable)");

            // ── Generic (compiler sees concrete type) ───────────
            group.subgroup("Generic");

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

            // ── Optimized (may_stop / active_stop) ──────────────
            group.subgroup("Optimized");

            group.bench("Option<None> (may_stop)", |b| {
                let stop: Option<&dyn Stop> = None;
                b.iter(|| {
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&stop).check();
                    }
                })
            });

            group.bench("Option<Some(Stopper)>", |b| {
                let stopper = Stopper::new();
                let stop: Option<&dyn Stop> = Some(&stopper);
                b.iter(|| {
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&stop).check();
                    }
                })
            });

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

            // ── Single dispatch (&dyn Stop, &DynStop) ───────────
            group.subgroup("Single dispatch");

            group.bench("&dyn Stop (Unstoppable)", |b| {
                let stop = Unstoppable;
                b.iter(|| check_100(&stop))
            });

            group.bench("&dyn Stop (Stopper)", |b| {
                let stop = Stopper::new();
                b.iter(|| check_100(&stop))
            });

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

            group.bench("DynStop owned (Stopper)", |b| {
                let stop = Stopper::new().into_dyn();
                b.iter(|| {
                    let stop = zenbench::black_box(&stop).clone();
                    for _ in 0..100 {
                        let _ = stop.check();
                    }
                })
            });

            // ── Double dispatch (wrapped behind &dyn Stop) ──────
            group.subgroup("Double dispatch");

            group.bench("&dyn Stop <- BoxedStop (Stopper)", |b| {
                let stop = Stopper::new().into_boxed();
                b.iter(|| check_100(&stop as &dyn Stop))
            });

            group.bench("&dyn Stop <- DynStop (Stopper)", |b| {
                let stop = Stopper::new().into_dyn();
                b.iter(|| check_100(&stop as &dyn Stop))
            });

            // ── Exotic types ────────────────────────────────────
            group.subgroup("Exotic");

            group.bench("WithTimeout", |b| {
                let source = StopSource::new();
                let stop = source.as_ref().with_timeout(Duration::from_secs(3600));
                b.iter(|| {
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&stop).check();
                    }
                })
            });

            group.bench("ChildStopper depth 3", |b| {
                let g0 = ChildStopper::new();
                let g1 = g0.child();
                let g2 = g1.child();
                let stop = g2.child();
                b.iter(|| {
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&stop).check();
                    }
                })
            });

            group.bench("OrStop<StopRef, StopRef>", |b| {
                let a = StopSource::new();
                let b_src = StopSource::new();
                let stop: OrStop<_, _> = a.as_ref().or(b_src.as_ref());
                b.iter(|| {
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&stop).check();
                    }
                })
            });
        });

        // ═══════════════════════════════════════════════════════════
        // Hot loops: 10k iterations, check() every 64.
        // Measures real-world overhead with work between checks.
        // ═══════════════════════════════════════════════════════════

        suite.compare("hot_loop_unstoppable", |group| {
            group.config().rounds(100).cache_firewall(false).sort_by_speed(true);
            group.baseline("generic");

            group.subgroup("Zero-cost paths");
            group.bench("generic", |b| {
                let stop = Unstoppable;
                b.iter(|| hot_loop!(stop))
            });

            group.subgroup("Optimized dyn");
            group.bench("&dyn Stop + may_stop", |b| {
                let stop = Unstoppable;
                let stop: &dyn Stop = &stop;
                let stop = stop.may_stop().then_some(stop);
                b.iter(|| hot_loop!(stop))
            });

            group.bench("DynStop.active_stop", |b| {
                let stop = Unstoppable.into_dyn();
                let stop = stop.active_stop();
                b.iter(|| hot_loop!(stop))
            });

            group.bench("BoxedStop.active_stop", |b| {
                let stop = Unstoppable.into_boxed();
                let stop = stop.active_stop();
                b.iter(|| hot_loop!(stop))
            });

            group.subgroup("Raw dyn (no optimization)");
            group.bench("&dyn Stop", |b| {
                let stop = Unstoppable;
                let stop: &dyn Stop = &stop;
                b.iter(|| hot_loop!(stop))
            });
        });

        suite.compare("hot_loop_stopper", |group| {
            group.config().rounds(100).cache_firewall(false).sort_by_speed(true);
            group.baseline("generic");

            group.subgroup("Direct");
            group.bench("generic", |b| {
                let stop = Stopper::new();
                b.iter(|| hot_loop!(stop))
            });

            group.subgroup("Optimized dyn");
            group.bench("&dyn Stop + may_stop", |b| {
                let stop = Stopper::new();
                let stop: &dyn Stop = &stop;
                let stop = stop.may_stop().then_some(stop);
                b.iter(|| hot_loop!(stop))
            });

            group.bench("DynStop.active_stop", |b| {
                let stop = Stopper::new().into_dyn();
                let stop = stop.active_stop();
                b.iter(|| hot_loop!(stop))
            });

            group.bench("BoxedStop.active_stop", |b| {
                let stop = Stopper::new().into_boxed();
                let stop = stop.active_stop();
                b.iter(|| hot_loop!(stop))
            });

            group.subgroup("Raw dyn (no optimization)");
            group.bench("&dyn Stop", |b| {
                let stop = Stopper::new();
                let stop: &dyn Stop = &stop;
                b.iter(|| hot_loop!(stop))
            });
        });

        // ═══════════════════════════════════════════════════════════
        // Error path + cold cache
        // ═══════════════════════════════════════════════════════════

        suite.compare("check_cancelled", |group| {
            group.config().rounds(200).cache_firewall(false);

            group.bench("Stopper", |b| {
                let stop = Stopper::cancelled();
                b.iter(|| zenbench::black_box(&stop).check())
            });

            group.bench("SyncStopper", |b| {
                let stop = SyncStopper::cancelled();
                b.iter(|| zenbench::black_box(&stop).check())
            });
        });

        suite.compare("cold_cache_stopper", |group| {
            group.config().rounds(100).cache_firewall(true).sort_by_speed(true);
            group.baseline("&dyn Stop");

            group.bench("&dyn Stop", |b| {
                let stop = Stopper::new();
                let stop: &dyn Stop = &stop;
                b.iter(|| hot_loop!(stop))
            });

            group.bench("DynStop.active_stop", |b| {
                let stop = Stopper::new().into_dyn();
                let stop = stop.active_stop();
                b.iter(|| hot_loop!(stop))
            });

            group.bench("BoxedStop.active_stop", |b| {
                let stop = Stopper::new().into_boxed();
                let stop = stop.active_stop();
                b.iter(|| hot_loop!(stop))
            });
        });
    });

    if let Err(e) = result.save("stop_check_zen_results.json") {
        eprintln!("Failed to save results: {e}");
    }
}

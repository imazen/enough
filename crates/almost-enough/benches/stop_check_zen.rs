//! Interleaved microbenchmarks for Stop dispatch patterns.
//!
//! Run with: cargo bench --bench stop_check_zen

use std::time::Duration;

use almost_enough::{
    ChildStopper, FnStop, OrStop, Stop, StopExt, StopSource, StopToken, Stopper, SyncStopper,
    TimeoutExt, Unstoppable,
};

const ITERS: usize = 10_000;

/// Work between checks — prevents loop elimination.
#[inline(always)]
fn work(acc: usize, i: usize) -> usize {
    acc.wrapping_add(zenbench::black_box(i.wrapping_mul(2654435761)))
}

/// Hot loop: check() on every iteration with trivial work.
/// No modulo gating — measures raw check() cost in a realistic loop.
#[inline(always)]
fn hot_loop(stop: &impl Stop) -> usize {
    let mut acc = 0usize;
    for i in 0..ITERS {
        let _ = stop.check();
        acc = work(acc, i);
    }
    zenbench::black_box(acc)
}

/// 100x check through &dyn Stop for per-call measurement.
#[inline(never)]
fn check_100(stop: &dyn Stop) {
    for _ in 0..100 {
        let _ = zenbench::black_box(stop).check();
    }
}

fn main() {
    let result = zenbench::run(|suite| {
        // ═══════════════════════════════════════════════════════════
        // Per-call cost: no-op stops (100x batched)
        // ═══════════════════════════════════════════════════════════

        suite.compare("noop_per_call", |group| {
            group
                .config()
                .max_rounds(200)
                .cache_firewall(false)
                .sort_by_speed(true);
            group.throughput(zenbench::Throughput::Elements(100));
            group.throughput_unit("checks");
            group.baseline("impl Stop (Unstoppable)");

            group.subgroup("Generic");
            group.bench("impl Stop (Unstoppable)", |b| {
                let stop = Unstoppable;
                b.iter(|| {
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&stop).check();
                    }
                })
            });

            group.subgroup("StopToken");
            group.bench("StopToken(Unstoppable)", |b| {
                let stop = StopToken::new(Unstoppable);
                b.iter(|| {
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&stop).check();
                    }
                })
            });

            group.subgroup("Unoptimized");
            group.bench("&dyn Stop (Unstoppable)", |b| {
                let stop = Unstoppable;
                b.iter(|| check_100(&stop))
            });
        });

        // ═══════════════════════════════════════════════════════════
        // Per-call cost: real stop sources (100x batched)
        // ═══════════════════════════════════════════════════════════

        suite.compare("real_per_call", |group| {
            group
                .config()
                .max_rounds(200)
                .cache_firewall(false)
                .sort_by_speed(true);
            group.throughput(zenbench::Throughput::Elements(100));
            group.throughput_unit("checks");
            group.baseline("impl Stop (Stopper)");

            group.subgroup("Generic");
            group.bench("impl Stop (Stopper)", |b| {
                let stop = Stopper::new();
                b.iter(|| {
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&stop).check();
                    }
                })
            });
            group.bench("impl Stop (FnStop)", |b| {
                let stop = FnStop::new(|| false);
                b.iter(|| {
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&stop).check();
                    }
                })
            });

            group.subgroup("StopToken");
            group.bench("StopToken(Stopper)", |b| {
                let stop = StopToken::new(Stopper::new());
                b.iter(|| {
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&stop).check();
                    }
                })
            });
            group.bench("StopToken(Stopper) via From", |b| {
                let stop: StopToken = Stopper::new().into();
                b.iter(|| {
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&stop).check();
                    }
                })
            });

            group.subgroup("Single dispatch");
            group.bench("&dyn Stop (Stopper)", |b| {
                let stop = Stopper::new();
                b.iter(|| check_100(&stop))
            });

            group.subgroup("Double dispatch");
            group.bench("&dyn Stop <- StopToken(Stopper)", |b| {
                let stop: StopToken = Stopper::new().into();
                b.iter(|| check_100(&stop as &dyn Stop))
            });

            group.subgroup("StopToken wrapping other types");
            group.bench("StopToken(FnStop) via &dyn", |b| {
                let stop = StopToken::new(FnStop::new(|| false));
                b.iter(|| check_100(&stop as &dyn Stop))
            });
            group.bench("StopToken(SyncStopper) via &dyn", |b| {
                let stop = StopToken::new(SyncStopper::new());
                b.iter(|| check_100(&stop as &dyn Stop))
            });
        });

        // ═══════════════════════════════════════════════════════════
        // Composite types (100x batched)
        // ═══════════════════════════════════════════════════════════

        suite.compare("composite_per_call", |group| {
            group
                .config()
                .max_rounds(200)
                .cache_firewall(false)
                .sort_by_speed(true);
            group.throughput(zenbench::Throughput::Elements(100));
            group.throughput_unit("checks");
            group.baseline("OrStop<StopRef, StopRef>");

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
            group.bench("ChildStopper (root)", |b| {
                let stop = ChildStopper::new();
                b.iter(|| {
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&stop).check();
                    }
                })
            });
            group.bench("ChildStopper (depth 3)", |b| {
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
            group.bench("WithTimeout (1h deadline)", |b| {
                let source = StopSource::new();
                let stop = source.as_ref().with_timeout(Duration::from_secs(3600));
                b.iter(|| {
                    for _ in 0..100 {
                        let _ = zenbench::black_box(&stop).check();
                    }
                })
            });
        });

        // ═══════════════════════════════════════════════════════════
        // Head-to-head: every path, same group, interleaved.
        // 10k iterations, check() every iteration, trivial work.
        // ═══════════════════════════════════════════════════════════

        suite.compare("head_to_head", |group| {
            group
                .config()
                .max_rounds(100)
                .cache_firewall(false)
                .sort_by_speed(true);
            group.baseline("generic Unstoppable");

            group.subgroup("Unstoppable paths");
            group.bench("generic Unstoppable", |b| {
                let stop = Unstoppable;
                b.iter(|| hot_loop(&stop))
            });
            group.bench("StopToken(Unstoppable)", |b| {
                let stop = StopToken::new(Unstoppable);
                b.iter(|| hot_loop(&stop))
            });
            group.bench("may_stop(StopToken(Unstoppable))", |b| {
                let stop = StopToken::new(Unstoppable);
                let stop = stop.may_stop().then_some(&stop);
                b.iter(|| hot_loop(&stop))
            });

            group.subgroup("Stopper paths");
            group.bench("generic Stopper", |b| {
                let stop = Stopper::new();
                b.iter(|| hot_loop(&stop))
            });
            group.bench("StopToken(Stopper)", |b| {
                let stop: StopToken = Stopper::new().into();
                b.iter(|| hot_loop(&stop))
            });
            group.bench("may_stop(StopToken(Stopper))", |b| {
                let stop: StopToken = Stopper::new().into();
                let stop = stop.may_stop().then_some(&stop);
                b.iter(|| hot_loop(&stop))
            });
            group.bench("&dyn Stop (Stopper)", |b| {
                let stop = Stopper::new();
                let stop: &dyn Stop = &stop;
                b.iter(|| hot_loop(&stop))
            });

            group.subgroup("Dyn variant (no specialization)");
            group.bench("StopToken(FnStop)", |b| {
                // FnStop hits the Dyn variant — no enum specialization
                let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let f = {
                    let flag = flag.clone();
                    FnStop::new(move || flag.load(std::sync::atomic::Ordering::Relaxed))
                };
                let stop = StopToken::new(f);
                b.iter(|| hot_loop(&stop))
            });
            group.bench("StopToken(StopSource)", |b| {
                // StopSource also hits Dyn variant
                let stop = StopToken::new(StopSource::new());
                b.iter(|| hot_loop(&stop))
            });
        });

        // ═══════════════════════════════════════════════════════════
        // Real-world: 64k pixels, check every 1024, ~5ns work per pixel
        // Models a codec inner loop (defilter, decode, etc.)
        // ═══════════════════════════════════════════════════════════

        suite.compare("real_world", |group| {
            group
                .config()
                .max_rounds(100)
                .cache_firewall(false)
                .sort_by_speed(true);
            group.baseline("no check (baseline work)");

            const PIXELS: usize = 65_536;
            const CHECK_EVERY: usize = 1024;

            // ~5ns of work per "pixel" — simulates a filter/decode step
            #[inline(always)]
            fn pixel_work(acc: u64, i: usize) -> u64 {
                let v = zenbench::black_box(i as u64);
                acc.wrapping_add(v.wrapping_mul(0x517cc1b727220a95))
                    .wrapping_add(v.rotate_left(13))
            }

            group.bench("no check (baseline work)", |b| {
                b.iter(|| {
                    let mut acc = 0u64;
                    for i in 0..PIXELS {
                        acc = pixel_work(acc, i);
                    }
                    zenbench::black_box(acc)
                })
            });

            group.bench("StopToken(Unstoppable)", |b| {
                let stop = StopToken::new(Unstoppable);
                b.iter(|| {
                    let mut acc = 0u64;
                    for i in 0..PIXELS {
                        if i % CHECK_EVERY == 0 {
                            let _ = stop.check();
                        }
                        acc = pixel_work(acc, i);
                    }
                    zenbench::black_box(acc)
                })
            });

            group.bench("StopToken(Stopper)", |b| {
                let stop: StopToken = Stopper::new().into();
                b.iter(|| {
                    let mut acc = 0u64;
                    for i in 0..PIXELS {
                        if i % CHECK_EVERY == 0 {
                            let _ = stop.check();
                        }
                        acc = pixel_work(acc, i);
                    }
                    zenbench::black_box(acc)
                })
            });

            group.bench("generic Stopper", |b| {
                let stop = Stopper::new();
                b.iter(|| {
                    let mut acc = 0u64;
                    for i in 0..PIXELS {
                        if i % CHECK_EVERY == 0 {
                            let _ = stop.check();
                        }
                        acc = pixel_work(acc, i);
                    }
                    zenbench::black_box(acc)
                })
            });

            group.bench("&dyn Stop (Stopper)", |b| {
                let stop = Stopper::new();
                let stop: &dyn Stop = &stop;
                b.iter(|| {
                    let mut acc = 0u64;
                    for i in 0..PIXELS {
                        if i % CHECK_EVERY == 0 {
                            let _ = stop.check();
                        }
                        acc = pixel_work(acc, i);
                    }
                    zenbench::black_box(acc)
                })
            });

            group.bench("StopToken(FnStop) [dyn variant]", |b| {
                let stop = StopToken::new(FnStop::new(|| false));
                b.iter(|| {
                    let mut acc = 0u64;
                    for i in 0..PIXELS {
                        if i % CHECK_EVERY == 0 {
                            let _ = stop.check();
                        }
                        acc = pixel_work(acc, i);
                    }
                    zenbench::black_box(acc)
                })
            });
        });

        // ═══════════════════════════════════════════════════════════
        // Pure check vs hot loop: does surrounding work change the picture?
        // ═══════════════════════════════════════════════════════════

        suite.compare("pure_vs_hot_loop", |group| {
            group
                .config()
                .max_rounds(100)
                .cache_firewall(false)
                .sort_by_speed(true);
            group.baseline("pure: StopToken(Stopper)");

            group.subgroup("Pure check (10k × check only)");
            group.bench("pure: StopToken(Stopper)", |b| {
                let stop: StopToken = Stopper::new().into();
                b.iter(|| {
                    for _ in 0..ITERS {
                        let _ = stop.check();
                    }
                })
            });
            group.bench("pure: StopToken(Unstoppable)", |b| {
                let stop = StopToken::new(Unstoppable);
                b.iter(|| {
                    for _ in 0..ITERS {
                        let _ = stop.check();
                    }
                })
            });
            group.bench("pure: generic Stopper", |b| {
                let stop = Stopper::new();
                b.iter(|| {
                    for _ in 0..ITERS {
                        let _ = stop.check();
                    }
                })
            });
            group.bench("pure: &dyn Stop (Stopper)", |b| {
                let stop = Stopper::new();
                let stop: &dyn Stop = &stop;
                b.iter(|| {
                    for _ in 0..ITERS {
                        let _ = stop.check();
                    }
                })
            });

            group.subgroup("Hot loop (10k × check + work)");
            group.bench("loop: StopToken(Stopper)", |b| {
                let stop: StopToken = Stopper::new().into();
                b.iter(|| hot_loop(&stop))
            });
            group.bench("loop: StopToken(Unstoppable)", |b| {
                let stop = StopToken::new(Unstoppable);
                b.iter(|| hot_loop(&stop))
            });
            group.bench("loop: generic Stopper", |b| {
                let stop = Stopper::new();
                b.iter(|| hot_loop(&stop))
            });
            group.bench("loop: &dyn Stop (Stopper)", |b| {
                let stop = Stopper::new();
                let stop: &dyn Stop = &stop;
                b.iter(|| hot_loop(&stop))
            });
        });

        // ═══════════════════════════════════════════════════════════
        // Error path + cold cache
        // ═══════════════════════════════════════════════════════════

        suite.compare("check_cancelled", |group| {
            group.config().max_rounds(200).cache_firewall(false);
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
            group
                .config()
                .max_rounds(100)
                .cache_firewall(true)
                .sort_by_speed(true);
            group.baseline("&dyn Stop (Stopper)");
            group.bench("&dyn Stop (Stopper)", |b| {
                let stop = Stopper::new();
                let stop: &dyn Stop = &stop;
                b.iter(|| hot_loop(&stop))
            });
            group.bench("StopToken(Stopper)", |b| {
                let stop: StopToken = Stopper::new().into();
                b.iter(|| hot_loop(&stop))
            });
        });
    });

    if let Err(e) = result.save("stop_check_zen_results.json") {
        eprintln!("Failed to save results: {e}");
    }
}

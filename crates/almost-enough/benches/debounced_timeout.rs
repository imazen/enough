//! Benchmarks for DebouncedTimeout vs WithTimeout.
//!
//! Measures the savings from skipping Instant::now() calls in various
//! workload profiles:
//!
//! 1. **Tight loop** (no work between checks) — maximum savings
//! 2. **Light work** (~50ns between checks) — moderate savings
//! 3. **Codec workload** (4KB between checks) — savings invisible
//! 4. **Calibration warmup** — cost of the initial learning phase
//!
//! Run with: cargo bench --bench debounced_timeout

use std::time::Duration;

use almost_enough::{DebouncedTimeoutExt, Stop, StopReason, Stopper, TimeoutExt};

// ── Workload helpers ───────────────────────────────────────────────

/// PNG Sub defilter (bpp=4) — real codec work, memory-bound.
#[inline(always)]
fn sub_defilter(buf: &mut [u8]) {
    for i in 4..buf.len() {
        buf[i] = buf[i].wrapping_add(buf[i - 4]);
    }
}

/// Light work: ~50ns of arithmetic per call.
#[inline(always)]
fn light_work(acc: &mut u64) {
    for _ in 0..10 {
        *acc = acc.wrapping_mul(6364136223846793005).wrapping_add(1);
    }
}

const BUF: usize = 256 * 1024;
const CHUNK: usize = 4096;

fn make_buf() -> Vec<u8> {
    (0..BUF)
        .map(|i| (i.wrapping_mul(0x9E3779B9) >> 24) as u8)
        .collect()
}

// ── Shared decode function (layout-immune via &dyn Stop) ──────────

#[inline(never)]
fn decode(buf: &mut [u8], stop: &dyn Stop) -> Result<(), StopReason> {
    for chunk in buf.chunks_mut(CHUNK) {
        stop.check()?;
        sub_defilter(chunk);
    }
    Ok(())
}

// ── Tight loop: 10K checks, no work between them ─────────────────

#[inline(never)]
fn check_10k(stop: &dyn Stop) -> Result<(), StopReason> {
    for _ in 0..10_000 {
        stop.check()?;
    }
    Ok(())
}

#[inline(never)]
fn check_10k_light(stop: &dyn Stop) -> Result<u64, StopReason> {
    let mut acc = 0u64;
    for _ in 0..10_000 {
        stop.check()?;
        light_work(&mut acc);
    }
    Ok(acc)
}

fn main() {
    let result = zenbench::run(|suite| {
        // ═══════════════════════════════════════════════════════════
        // 1. Tight loop: no work between checks
        //
        // This is the best case for DebouncedTimeout. WithTimeout
        // calls Instant::now() on every check (~15-25ns). Debounced
        // skips most of them.
        // ═══════════════════════════════════════════════════════════

        suite.compare("tight_loop_10k", |group| {
            group.config().sort_by_speed(true).cache_firewall(false);
            group.baseline("WithTimeout");
            group.throughput(zenbench::Throughput::Elements(10_000));
            group.throughput_unit("checks");

            group.bench("WithTimeout", |b| {
                let stop = Stopper::new();
                let timeout = stop.with_timeout(Duration::from_secs(60));
                b.iter(|| check_10k(&timeout))
            });

            group.bench("DebouncedTimeout", |b| {
                let stop = Stopper::new();
                let timeout = stop.with_debounced_timeout(Duration::from_secs(60));
                // Pre-warm calibration
                for _ in 0..20_000 {
                    let _ = timeout.check();
                }
                b.iter(|| check_10k(&timeout))
            });

            group.bench("Stopper (no timeout)", |b| {
                let stop = Stopper::new();
                b.iter(|| check_10k(&stop))
            });
        });

        // ═══════════════════════════════════════════════════════════
        // 2. Light work: ~50ns between checks
        //
        // More realistic than pure check() overhead. DebouncedTimeout
        // should still show measurable savings since 50ns work + 25ns
        // Instant::now() is ~33% overhead.
        // ═══════════════════════════════════════════════════════════

        suite.compare("light_work_10k", |group| {
            group.config().sort_by_speed(true).cache_firewall(false);
            group.baseline("WithTimeout");
            group.throughput(zenbench::Throughput::Elements(10_000));
            group.throughput_unit("checks");

            group.bench("WithTimeout", |b| {
                let stop = Stopper::new();
                let timeout = stop.with_timeout(Duration::from_secs(60));
                b.iter(|| check_10k_light(&timeout))
            });

            group.bench("DebouncedTimeout", |b| {
                let stop = Stopper::new();
                let timeout = stop.with_debounced_timeout(Duration::from_secs(60));
                for _ in 0..20_000 {
                    let _ = timeout.check();
                }
                b.iter(|| check_10k_light(&timeout))
            });

            group.bench("Stopper (no timeout)", |b| {
                let stop = Stopper::new();
                b.iter(|| check_10k_light(&stop))
            });
        });

        // ═══════════════════════════════════════════════════════════
        // 3. Codec workload: 4KB between checks
        //
        // The real work (~7μs per 4KB chunk) dominates. Both timeout
        // types should be equivalent. This confirms DebouncedTimeout
        // adds no overhead compared to WithTimeout in normal usage.
        // ═══════════════════════════════════════════════════════════

        suite.compare("codec_256kb", |group| {
            group.config().sort_by_speed(true).cache_firewall(false);
            group.baseline("WithTimeout");
            group.throughput(zenbench::Throughput::Bytes(BUF as u64));

            group.bench("WithTimeout", |b| {
                let stop = Stopper::new();
                let timeout = stop.with_timeout(Duration::from_secs(60));
                let mut work = make_buf();
                b.iter(|| {
                    let _ = decode(&mut work, &timeout);
                    zenbench::black_box(&work);
                })
            });

            group.bench("DebouncedTimeout", |b| {
                let stop = Stopper::new();
                let timeout = stop.with_debounced_timeout(Duration::from_secs(60));
                let mut work = make_buf();
                // Pre-warm with codec-rate calls
                for chunk in work.chunks_mut(CHUNK) {
                    let _ = timeout.check();
                    zenbench::black_box(chunk);
                }
                b.iter(|| {
                    let _ = decode(&mut work, &timeout);
                    zenbench::black_box(&work);
                })
            });

            group.bench("Stopper (no timeout)", |b| {
                let stop = Stopper::new();
                let mut work = make_buf();
                b.iter(|| {
                    let _ = decode(&mut work, &stop);
                    zenbench::black_box(&work);
                })
            });
        });

        // ═══════════════════════════════════════════════════════════
        // 4. Calibration warmup: cold start behavior
        //
        // How much does the initial calibration phase cost? We create
        // a fresh DebouncedTimeout and measure the first 10K calls
        // (which include calibration overhead).
        // ═══════════════════════════════════════════════════════════

        suite.compare("calibration_warmup", |group| {
            group.config().sort_by_speed(true).cache_firewall(false);
            group.baseline("WithTimeout");
            group.throughput(zenbench::Throughput::Elements(10_000));
            group.throughput_unit("checks");

            group.bench("WithTimeout", |b| {
                let stop = Stopper::new();
                let timeout = stop.with_timeout(Duration::from_secs(60));
                b.iter(|| check_10k(&timeout))
            });

            group.bench("DebouncedTimeout (cold)", |b| {
                b.iter(|| {
                    // Fresh each iteration — includes calibration cost
                    let stop = Stopper::new();
                    let timeout = stop.with_debounced_timeout(Duration::from_secs(60));
                    check_10k(&timeout)
                })
            });

            group.bench("DebouncedTimeout (warm)", |b| {
                let stop = Stopper::new();
                let timeout = stop.with_debounced_timeout(Duration::from_secs(60));
                for _ in 0..20_000 {
                    let _ = timeout.check();
                }
                b.iter(|| check_10k(&timeout))
            });
        });

        // ═══════════════════════════════════════════════════════════
        // 5. Target interval sensitivity
        //
        // How does the target check interval affect performance?
        // Smaller targets = more clock reads = more overhead.
        // ═══════════════════════════════════════════════════════════

        suite.compare("target_interval", |group| {
            group.config().sort_by_speed(true).cache_firewall(false);
            group.baseline("WithTimeout (always)");
            group.throughput(zenbench::Throughput::Elements(10_000));
            group.throughput_unit("checks");

            group.bench("WithTimeout (always)", |b| {
                let stop = Stopper::new();
                let timeout = stop.with_timeout(Duration::from_secs(60));
                b.iter(|| check_10k(&timeout))
            });

            for target_us in [10, 50, 100, 500, 1000] {
                group.bench(format!("Debounced ({target_us}μs)"), move |b| {
                    let stop = Stopper::new();
                    let timeout = stop
                        .with_debounced_timeout(Duration::from_secs(60))
                        .with_target_interval(Duration::from_micros(target_us));
                    for _ in 0..50_000 {
                        let _ = timeout.check();
                    }
                    b.iter(|| check_10k(&timeout))
                });
            }
        });
    });

    if let Err(e) = result.save("debounced_timeout_results.json") {
        eprintln!("Failed to save results: {e}");
    }
}

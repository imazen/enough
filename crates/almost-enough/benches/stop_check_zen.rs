//! Cancellation overhead benchmarks for enough/almost-enough.
//!
//! Answers the questions library authors actually ask:
//! 1. Does adding cancellation checking slow down my codec? (No.)
//! 2. Which API signature should I use? (Doesn't matter.)
//! 3. What's the raw per-check cost? (Sub-ns to ~1ns, invisible in practice.)
//!
//! IMPORTANT: The codec benchmarks use a single `#[inline(never)]` function
//! called through a function pointer to ensure ALL variants share the same
//! code layout. Without this, instruction cache alignment can cause 2x
//! swings between identical loops (confirmed empirically — see Mytkowicz,
//! ASPLOS 2009). By routing every variant through the same function body,
//! we eliminate layout bias and measure only the check() dispatch cost.
//!
//! Run with: cargo bench --bench stop_check_zen

use almost_enough::{FnStop, Stop, StopReason, StopToken, Stopper, SyncStopper, Unstoppable};

/// 256KB — fits L2, realistic for one image tile/row batch.
const BUF: usize = 256 * 1024;
/// Check every 4KB — typical for per-row cancellation in codecs.
/// 256KB / 4KB = 64 checks per decode call.
const CHUNK: usize = 4096;

/// PNG Sub defilter (bpp=4): each byte += byte 4 positions back.
/// Memory-bound with a carried dependency chain — this is actual
/// codec work, not a synthetic arithmetic stand-in.
#[inline(always)]
fn sub_defilter(buf: &mut [u8]) {
    for i in 4..buf.len() {
        buf[i] = buf[i].wrapping_add(buf[i - 4]);
    }
}

fn make_buf() -> Vec<u8> {
    (0..BUF).map(|i| (i.wrapping_mul(0x9E3779B9) >> 24) as u8).collect()
}

// ── Single decode function for fair comparison ──────────────────────
// Using ONE function with &dyn Stop eliminates code layout bias.
// All variants execute the same instruction addresses — the only
// difference is what check() dispatches to through the vtable/enum.

#[inline(never)]
fn decode(buf: &mut [u8], stop: &dyn Stop) -> Result<(), StopReason> {
    for chunk in buf.chunks_mut(CHUNK) {
        stop.check()?;
        sub_defilter(chunk);
    }
    Ok(())
}

// ── Separate decode functions for API signature comparison ──────────
// These DO have different code layout, but the per_check_isolated group
// (which is layout-insensitive) confirms the relative costs.

#[inline(never)]
fn decode_token(buf: &mut [u8], stop: &StopToken) -> Result<(), StopReason> {
    for chunk in buf.chunks_mut(CHUNK) {
        stop.check()?;
        sub_defilter(chunk);
    }
    Ok(())
}

// ── Check-only functions for isolated overhead measurement ──────────
// #[inline(never)] prevents the compiler from specializing on the
// concrete type — matches real API usage where check() crosses a
// library boundary.

#[inline(never)]
fn check_10k_dyn(stop: &dyn Stop) -> Result<(), StopReason> {
    for _ in 0..10_000 {
        stop.check()?;
    }
    Ok(())
}

#[inline(never)]
fn check_10k_token(stop: &StopToken) -> Result<(), StopReason> {
    for _ in 0..10_000 {
        stop.check()?;
    }
    Ok(())
}

#[inline(never)]
fn check_10k_generic(stop: &impl Stop) -> Result<(), StopReason> {
    for _ in 0..10_000 {
        stop.check()?;
    }
    Ok(())
}

fn main() {
    let result = zenbench::run(|suite| {
        // ═══════════════════════════════════════════════════════════
        // 1. Does cancellation slow down real work?
        //
        // 256KB PNG Sub defilter, check every 4KB (64 checks/call).
        // ALL variants call the same decode() function — only the
        // &dyn Stop vtable target differs. This eliminates code
        // layout bias that can otherwise cause 2x swings.
        // ═══════════════════════════════════════════════════════════

        suite.compare("codec_overhead", |group| {
            group.config().sort_by_speed(true).cache_firewall(false);
            group.baseline("Unstoppable");
            group.throughput(zenbench::Throughput::Bytes(BUF as u64));

            group.bench("Unstoppable", |b| {
                let mut work = make_buf();
                b.iter(|| {
                    let _ = decode(&mut work, &Unstoppable);
                    zenbench::black_box(&work);
                })
            });

            group.bench("Stopper", |b| {
                let stop = Stopper::new();
                let mut work = make_buf();
                b.iter(|| {
                    let _ = decode(&mut work, &stop);
                    zenbench::black_box(&work);
                })
            });

            group.bench("SyncStopper", |b| {
                let stop = SyncStopper::new();
                let mut work = make_buf();
                b.iter(|| {
                    let _ = decode(&mut work, &stop);
                    zenbench::black_box(&work);
                })
            });

            group.bench("FnStop(|| false)", |b| {
                let stop = FnStop::new(|| false);
                let mut work = make_buf();
                b.iter(|| {
                    let _ = decode(&mut work, &stop);
                    zenbench::black_box(&work);
                })
            });

            group.bench("StopToken(Stopper)", |b| {
                let stop: StopToken = Stopper::new().into();
                let mut work = make_buf();
                b.iter(|| {
                    let _ = decode(&mut work, &stop);
                    zenbench::black_box(&work);
                })
            });
        });

        // ═══════════════════════════════════════════════════════════
        // 2. StopToken vs &dyn Stop: does the wrapper matter?
        //
        // StopToken has specialized enum paths for Stopper and
        // SyncStopper. This group tests whether that specialization
        // is visible through real work. Uses decode_token() which
        // is a separate function — layout may differ from decode(),
        // so compare within this group, not across groups.
        // ═══════════════════════════════════════════════════════════

        suite.compare("stoptoken_dispatch", |group| {
            group.config().sort_by_speed(true).cache_firewall(false);
            group.baseline("token(Stopper)");
            group.throughput(zenbench::Throughput::Bytes(BUF as u64));

            group.bench("token(Stopper)", |b| {
                let stop: StopToken = Stopper::new().into();
                let mut work = make_buf();
                b.iter(|| {
                    let _ = decode_token(&mut work, &stop);
                    zenbench::black_box(&work);
                })
            });

            group.bench("token(SyncStopper)", |b| {
                let stop: StopToken = SyncStopper::new().into();
                let mut work = make_buf();
                b.iter(|| {
                    let _ = decode_token(&mut work, &stop);
                    zenbench::black_box(&work);
                })
            });

            group.bench("token(FnStop)", |b| {
                let stop = StopToken::new(FnStop::new(|| false));
                let mut work = make_buf();
                b.iter(|| {
                    let _ = decode_token(&mut work, &stop);
                    zenbench::black_box(&work);
                })
            });

            group.bench("token(Unstoppable)", |b| {
                let stop = StopToken::new(Unstoppable);
                let mut work = make_buf();
                b.iter(|| {
                    let _ = decode_token(&mut work, &stop);
                    zenbench::black_box(&work);
                })
            });
        });

        // ═══════════════════════════════════════════════════════════
        // 3. Isolated per-check cost
        //
        // 10K check() calls behind #[inline(never)] — the compiler
        // cannot see the concrete type, matching real API usage.
        //
        // These numbers are real but invisible in practice: group 1
        // proves the overhead vanishes into real codec work. Shown
        // for transparency and documentation.
        // ═══════════════════════════════════════════════════════════

        suite.compare("per_check_isolated", |group| {
            group.config().sort_by_speed(true).cache_firewall(false);
            group.throughput(zenbench::Throughput::Elements(10_000));
            group.throughput_unit("checks");
            group.baseline("&dyn (Stopper)");

            group.subgroup("StopToken (specialized)");
            group.bench("token(Unstoppable)", |b| {
                let stop = StopToken::new(Unstoppable);
                b.iter(|| check_10k_token(&stop))
            });
            group.bench("token(Stopper)", |b| {
                let stop: StopToken = Stopper::new().into();
                b.iter(|| check_10k_token(&stop))
            });
            group.bench("token(SyncStopper)", |b| {
                let stop: StopToken = SyncStopper::new().into();
                b.iter(|| check_10k_token(&stop))
            });

            group.subgroup("StopToken (dyn fallback)");
            group.bench("token(FnStop)", |b| {
                let stop = StopToken::new(FnStop::new(|| false));
                b.iter(|| check_10k_token(&stop))
            });

            group.subgroup("&dyn Stop (vtable)");
            group.bench("&dyn (Stopper)", |b| {
                let stop = Stopper::new();
                b.iter(|| check_10k_dyn(&stop))
            });
            group.bench("&dyn (Unstoppable)", |b| {
                b.iter(|| check_10k_dyn(&Unstoppable))
            });

            group.subgroup("impl Stop (monomorphized)");
            group.bench("impl (Stopper)", |b| {
                let stop = Stopper::new();
                b.iter(|| check_10k_generic(&stop))
            });
            group.bench("impl (Unstoppable)", |b| {
                b.iter(|| check_10k_generic(&Unstoppable))
            });
        });

        // ═══════════════════════════════════════════════════════════
        // 4. Cold cache: codec decode after L2 flush
        //
        // Cache firewall evicts 2MB between samples. Both the stop
        // source and the 256KB work buffer start cold, simulating
        // a decode call after a context switch.
        // ═══════════════════════════════════════════════════════════

        suite.compare("cold_cache", |group| {
            group.config().sort_by_speed(true).cache_firewall(true);
            group.baseline("Unstoppable");
            group.throughput(zenbench::Throughput::Bytes(BUF as u64));

            group.bench("Unstoppable", |b| {
                let mut work = make_buf();
                b.iter(|| {
                    let _ = decode(&mut work, &Unstoppable);
                    zenbench::black_box(&work);
                })
            });

            group.bench("Stopper", |b| {
                let stop = Stopper::new();
                let mut work = make_buf();
                b.iter(|| {
                    let _ = decode(&mut work, &stop);
                    zenbench::black_box(&work);
                })
            });

            group.bench("StopToken(Stopper)", |b| {
                let stop: StopToken = Stopper::new().into();
                let mut work = make_buf();
                b.iter(|| {
                    let _ = decode(&mut work, &stop);
                    zenbench::black_box(&work);
                })
            });
        });
    });

    if let Err(e) = result.save("stop_check_zen_results.json") {
        eprintln!("Failed to save results: {e}");
    }
}

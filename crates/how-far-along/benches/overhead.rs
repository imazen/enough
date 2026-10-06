//! What a checkpoint costs, alone and inside real work.
//!
//! Questions this answers:
//! 1. What does one `check()` or `advance()` through `&dyn Pulse` cost?
//! 2. Does checkpoint traffic slow a realistic codec loop, and how does the
//!    answer change with checkpoint frequency?
//! 3. Does dynamic dispatch cost more than a monomorphized `impl Pulse`?
//!
//! The work functions are `#[inline(never)]` and shared by every variant, so
//! variants differ only in what the vtable calls (see almost-enough's
//! `stop_check_zen` bench for why shared code layout matters).
//!
//! Run with `cargo bench -p how-far-along --bench overhead`.

use almost_enough::Stopper;
use how_far::NoReport;
use how_far_along::ext::ReportExt;
use how_far_along::{
    NoPulse, Phase, ProgressExt, Pulse, PulseTree, Report, Stop, StopReason, Total, Unstoppable,
};
use std::num::NonZeroU64;

/// 256 KiB, one image tile or row batch.
const BUF: usize = 256 * 1024;

fn make_buf() -> Vec<u8> {
    (0..BUF)
        .map(|i| (i.wrapping_mul(0x9E37_79B9) >> 24) as u8)
        .collect()
}

/// PNG Sub defilter with 4 bytes per pixel: memory-bound with a carried
/// dependency, the kind of loop codecs checkpoint around.
#[inline(always)]
fn sub_defilter(buf: &mut [u8]) {
    for i in 4..buf.len() {
        buf[i] = buf[i].wrapping_add(buf[i - 4]);
    }
}

/// Check and count once per `chunk` bytes, through one vtable.
#[inline(never)]
fn decode(buf: &mut [u8], chunk: usize, pulse: &dyn Pulse) -> Result<(), StopReason> {
    pulse.check()?;
    for part in buf.chunks_mut(chunk) {
        sub_defilter(part);
        pulse.step(part.len() as u64)?;
    }
    Ok(())
}

/// The same loop, monomorphized for one pulse type.
#[inline(never)]
fn decode_generic<P: Pulse>(buf: &mut [u8], chunk: usize, pulse: &P) -> Result<(), StopReason> {
    pulse.check()?;
    for part in buf.chunks_mut(chunk) {
        sub_defilter(part);
        pulse.step(part.len() as u64)?;
    }
    Ok(())
}

/// The `&dyn` loop gated with `ProgressExt::live`: no calls at all when the
/// pulse neither stops nor reports.
#[inline(never)]
fn decode_live(buf: &mut [u8], chunk: usize, pulse: &dyn Pulse) -> Result<(), StopReason> {
    let pulse = pulse.live();
    pulse.check()?;
    for part in buf.chunks_mut(chunk) {
        sub_defilter(part);
        pulse.step(part.len() as u64)?;
    }
    Ok(())
}

#[inline(never)]
fn check_10k(pulse: &dyn Pulse) -> Result<(), StopReason> {
    for _ in 0..10_000 {
        pulse.check()?;
    }
    Ok(())
}

#[inline(never)]
fn check_10k_gated(pulse: &dyn Pulse) -> Result<(), StopReason> {
    // The `may_stop` gate: no calls at all when the pulse can never stop.
    let stop = pulse.may_stop().then_some(pulse);
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

#[inline(never)]
fn advance_10k(report: &dyn Report) {
    for _ in 0..10_000 {
        report.advance(zenbench::black_box(1));
    }
}

/// Time `decode` over one 256 KiB buffer, reused across iterations.
fn on_buf(b: &mut zenbench::Bencher, mut decode: impl FnMut(&mut [u8]) -> Result<(), StopReason>) {
    let mut work = make_buf();
    b.iter(|| {
        let _ = decode(&mut work);
        zenbench::black_box(&work);
    })
}

fn tree(stop: impl Stop + 'static) -> PulseTree {
    PulseTree::new(Phase::new("bench", Total::Unknown), stop)
}

fn main() {
    // The resource gate is off because zenbench 0.1.9 on Linux mistakes its
    // own lock-heartbeat thread for a concurrent benchmark and waits 30 s per
    // round. Interleaved rounds and paired statistics still keep comparisons
    // within a group fair; run on a quiet machine for absolute numbers.
    zenbench::run_gated(zenbench::GateConfig::disabled(), |suite| {
        // ═══════════════════════════════════════════════════════════════
        // 1. One checkpoint, isolated. 10k calls behind `#[inline(never)]`.
        // ═══════════════════════════════════════════════════════════════
        suite.compare("check_isolated", |group| {
            group.config().sort_by_speed(true).cache_firewall(false);
            group.throughput(zenbench::Throughput::Elements(10_000));
            group.throughput_unit("checks");
            group.baseline("&dyn Pulse → NoPulse");
            group.bench("&dyn Pulse → NoPulse", |b| b.iter(|| check_10k(&NoPulse)));
            group.bench("&dyn Pulse → tree(Unstoppable)", |b| {
                let pulse = tree(Unstoppable);
                b.iter(|| check_10k(&pulse))
            });
            group.bench("&dyn Pulse → tree(Stopper)", |b| {
                let pulse = tree(Stopper::new());
                b.iter(|| check_10k(&pulse))
            });
            group.bench("may_stop gate → tree(Unstoppable)", |b| {
                let pulse = tree(Unstoppable);
                b.iter(|| check_10k_gated(&pulse))
            });
            group.bench("impl Stop → Unstoppable", |b| {
                b.iter(|| check_10k_generic(&Unstoppable))
            });
        });

        suite.compare("advance_isolated", |group| {
            group.config().sort_by_speed(true).cache_firewall(false);
            group.throughput(zenbench::Throughput::Elements(10_000));
            group.throughput_unit("reports");
            group.baseline("&dyn Report → NoReport");
            group.bench("&dyn Report → NoReport", |b| {
                b.iter(|| advance_10k(&NoReport))
            });
            group.bench("&dyn Report → Reporter", |b| {
                let phase = Phase::new("bench", Total::Unknown);
                let reporter = phase.reporter();
                b.iter(|| advance_10k(&reporter))
            });
            group.bench("&dyn Pulse → tree leaf", |b| {
                let pulse = tree(Unstoppable);
                b.iter(|| advance_10k(&pulse))
            });
            group.bench("Batch(64) → Reporter", |b| {
                let phase = Phase::new("bench", Total::Unknown);
                let mut batch = phase.reporter().batched(NonZeroU64::new(64).unwrap());
                b.iter(|| {
                    for _ in 0..10_000 {
                        batch.advance(zenbench::black_box(1));
                    }
                })
            });
        });

        // ═══════════════════════════════════════════════════════════════
        // 2. Inside real work: 256 KiB defilter, check + step per chunk.
        //    Same function body for every pulse; only the vtable differs.
        // ═══════════════════════════════════════════════════════════════
        for chunk in [4096_usize, 256, 64] {
            suite.compare(format!("codec_loop_chunk_{chunk}"), |group| {
                group.config().sort_by_speed(true).cache_firewall(false);
                group.throughput(zenbench::Throughput::Bytes(BUF as u64));
                group.baseline("NoPulse");
                group.bench("NoPulse", move |b| {
                    on_buf(b, |w| decode(w, chunk, &NoPulse))
                });
                group.bench("tree(Unstoppable)", move |b| {
                    let pulse = tree(Unstoppable);
                    on_buf(b, |w| decode(w, chunk, &pulse))
                });
                group.bench("tree(Stopper)", move |b| {
                    let pulse = tree(Stopper::new());
                    on_buf(b, |w| decode(w, chunk, &pulse))
                });
            });
        }

        // ═══════════════════════════════════════════════════════════════
        // 3. Dynamic versus monomorphized dispatch in the same loop.
        //    Different function bodies, so compare within this group only.
        // ═══════════════════════════════════════════════════════════════
        suite.compare("dyn_vs_generic_chunk_256", |group| {
            group.config().sort_by_speed(true).cache_firewall(false);
            group.throughput(zenbench::Throughput::Bytes(BUF as u64));
            group.baseline("&dyn Pulse → tree");
            group.bench("&dyn Pulse → tree", |b| {
                let pulse = tree(Stopper::new());
                on_buf(b, |w| decode(w, 256, &pulse))
            });
            group.bench("impl Pulse → tree", |b| {
                let pulse = tree(Stopper::new());
                on_buf(b, |w| decode_generic(w, 256, &pulse))
            });
            group.bench("impl Pulse → NoPulse", |b| {
                on_buf(b, |w| decode_generic(w, 256, &NoPulse))
            });
            group.bench("gated &dyn Pulse → NoPulse", |b| {
                on_buf(b, |w| decode_live(w, 256, &NoPulse))
            });
            group.bench("gated &dyn Pulse → tree", |b| {
                let pulse = tree(Stopper::new());
                on_buf(b, |w| decode_live(w, 256, &pulse))
            });
        });
    });
}

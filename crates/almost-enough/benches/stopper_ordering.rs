//! What Release/Acquire ordering costs: `SyncStopper` against `Stopper`,
//! checked alone and from loops with real work between checks.
//!
//! Run with: cargo bench -p almost-enough --bench stopper_ordering
//!
//! The resource gate is disabled: zenbench 0.1.9 counts its own lock thread
//! as a competing benchmark on Linux and waits 30 s per round. The pairs are
//! interleaved, so machine load affects both sides alike.

use std::hint::black_box;

use almost_enough::{Stop, StopReason, Stopper, SyncStopper};

const BUFFER: usize = 64 * 1024;

#[inline(never)]
fn check_dyn(stop: &dyn Stop) -> Result<(), StopReason> {
    stop.check()
}

#[inline(never)]
fn check_generic<S: Stop>(stop: &S) -> Result<(), StopReason> {
    stop.check()
}

/// PNG Sub defilter over `buf`, checking `stop` after every `chunk` bytes.
#[inline(never)]
fn defilter(buf: &mut [u8], chunk: usize, stop: &dyn Stop) -> Result<(), StopReason> {
    for part in buf.chunks_mut(chunk) {
        for i in 4..part.len() {
            part[i] = part[i].wrapping_add(part[i - 4]);
        }
        stop.check()?;
    }
    Ok(())
}

fn main() {
    let result = zenbench::run_gated(zenbench::GateConfig::disabled(), |suite| {
        suite.compare("check through &dyn Stop", |group| {
            group.config().cache_firewall(false);
            group.baseline("stopper");
            group.bench("stopper", |b| {
                let stop = Stopper::new();
                b.iter(|| check_dyn(black_box(&stop)))
            });
            group.bench("sync_stopper", |b| {
                let stop = SyncStopper::new();
                b.iter(|| check_dyn(black_box(&stop)))
            });
        });

        suite.compare("check, generic", |group| {
            group.config().cache_firewall(false);
            group.baseline("stopper");
            group.bench("stopper", |b| {
                let stop = Stopper::new();
                b.iter(|| check_generic(black_box(&stop)))
            });
            group.bench("sync_stopper", |b| {
                let stop = SyncStopper::new();
                b.iter(|| check_generic(black_box(&stop)))
            });
        });

        for chunk in [64, 1024] {
            suite.compare(format!("defilter 64 KiB, check every {chunk} B"), |group| {
                group.config().cache_firewall(false);
                group.baseline("stopper");
                group.throughput(zenbench::Throughput::Bytes(BUFFER as u64));
                group.bench("stopper", move |b| {
                    let stop = Stopper::new();
                    let mut buf = vec![7u8; BUFFER];
                    b.iter(|| defilter(black_box(&mut buf), chunk, black_box(&stop)))
                });
                group.bench("sync_stopper", move |b| {
                    let stop = SyncStopper::new();
                    let mut buf = vec![7u8; BUFFER];
                    b.iter(|| defilter(black_box(&mut buf), chunk, black_box(&stop)))
                });
            });
        }
    });

    if let Err(e) = result.save("stopper_ordering_results.json") {
        eprintln!("Failed to save results: {e}");
    }
}

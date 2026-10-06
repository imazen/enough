//! What a `ChildStopper` check costs as the tree deepens, against a
//! `Stopper`: checked alone (through `&dyn Stop` and generically) and from a
//! loop with 1 KiB of work between checks.
//!
//! Run with: cargo bench -p almost-enough --bench child_stopper
//!
//! The resource gate is disabled: zenbench 0.1.9 counts its own lock thread
//! as a competing benchmark on Linux and waits 30 s per round.

use std::hint::black_box;

use almost_enough::{ChildStopper, Stop, StopReason, Stopper};

const BUFFER: usize = 64 * 1024;

/// A node `depth` levels down a tree of `ChildStopper`s (1 is the root).
fn chain(depth: usize) -> ChildStopper {
    let mut node = ChildStopper::new();
    for _ in 1..depth {
        node = node.child();
    }
    node
}

/// A node `depth` levels down a tree whose root is under a `Stopper`.
fn chain_on_stopper(depth: usize) -> ChildStopper {
    let mut node = ChildStopper::with_parent(Stopper::new());
    for _ in 1..depth {
        node = node.child();
    }
    node
}

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
            for depth in [1, 2, 4, 8] {
                group.bench(format!("child, depth {depth}"), move |b| {
                    let stop = chain(depth);
                    b.iter(|| check_dyn(black_box(&stop)))
                });
            }
            group.bench("child under a Stopper, depth 4", |b| {
                let stop = chain_on_stopper(4);
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
            for depth in [1, 2, 4, 8] {
                group.bench(format!("child, depth {depth}"), move |b| {
                    let stop = chain(depth);
                    b.iter(|| check_generic(black_box(&stop)))
                });
            }
        });

        suite.compare("defilter 64 KiB, check every 1 KiB", |group| {
            group.config().cache_firewall(false);
            group.baseline("stopper");
            group.throughput(zenbench::Throughput::Bytes(BUFFER as u64));
            group.bench("stopper", |b| {
                let stop = Stopper::new();
                let mut buf = vec![7u8; BUFFER];
                b.iter(|| defilter(black_box(&mut buf), 1024, black_box(&stop)))
            });
            for depth in [2, 8] {
                group.bench(format!("child, depth {depth}"), move |b| {
                    let stop = chain(depth);
                    let mut buf = vec![7u8; BUFFER];
                    b.iter(|| defilter(black_box(&mut buf), 1024, black_box(&stop)))
                });
            }
        });
    });

    if let Err(e) = result.save("child_stopper_results.json") {
        eprintln!("Failed to save results: {e}");
    }
}

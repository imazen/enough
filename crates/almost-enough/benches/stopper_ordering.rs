//! What Release/Acquire ordering costs. `Stopper` (an Acquire load per
//! check) against three flags laid out as it is, an `Arc` holding an
//! `AtomicBool`, that differ only in the check:
//! - `relaxed_flag`: a Relaxed load, what `Stopper` was before 0.4.5;
//! - `acquire_flag`: an Acquire load, the same instructions as `Stopper`,
//!   so its gap to `stopper` shows how much code placement alone moves a row;
//! - `fenced_flag`: a Relaxed load and an Acquire fence once it reads
//!   `true`, an alternative that was measured and rejected.
//!
//! Checked alone, from loops with real work between checks, and from loops
//! bound by memory latency, where an acquire load could keep the loads after
//! a check from overlapping it. On aarch64 the Acquire load is `ldapr` where
//! the target enables RCpc (`aarch64-apple-darwin` does) and `ldar` where it
//! doesn't (the Linux, Windows, Android and iOS targets); `-C target-cpu`
//! can change which.
//!
//! Run with: cargo bench -p almost-enough --bench stopper_ordering
//!
//! The resource gate is disabled: zenbench 0.1.9 counts its own lock thread
//! as a competing benchmark on Linux and waits 30 s per round. The pairs are
//! interleaved, so machine load affects both sides alike.

use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use almost_enough::{Stop, StopReason, Stopper};

const BUFFER: usize = 64 * 1024;

/// A flag with Relaxed ordering on both sides, laid out as `Stopper` was
/// before 0.4.5: an `Arc` holding an `AtomicBool`.
#[derive(Clone)]
struct RelaxedFlag(Arc<AtomicBool>);

impl RelaxedFlag {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}

impl Stop for RelaxedFlag {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        if self.0.load(Ordering::Relaxed) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// A flag checked with an Acquire load, as `Stopper` is.
#[derive(Clone)]
struct AcquireFlag(Arc<AtomicBool>);

impl AcquireFlag {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}

impl Stop for AcquireFlag {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        if self.0.load(Ordering::Acquire) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// A flag checked with a Relaxed load and an Acquire fence once it reads
/// `true`. Same guarantee as an Acquire load, and a plain load until the
/// stop; measured no cheaper than `ldar` on Neoverse-N1, and a branch more
/// than an Acquire load on x86-64, where that load is free.
#[derive(Clone)]
struct FencedFlag(Arc<AtomicBool>);

impl FencedFlag {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}

impl Stop for FencedFlag {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        if self.0.load(Ordering::Relaxed) {
            std::sync::atomic::fence(Ordering::Acquire);
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Entries in the memory-bound buffers: 256 MiB of `u64` and of `u32`, far
/// beyond the last-level cache of every machine measured.
const GATHER_ENTRIES: usize = 32 << 20;
const CHASE_ENTRIES: usize = 64 << 20;
/// Loads per iteration of a memory-bound loop.
const LOADS: usize = 1 << 12;

/// `LOADS` independent loads from random places in `data` (a power-of-two
/// length), checking `stop` every `every` loads (a power of two). The
/// addresses come from a PRNG, not from loaded data, so cache misses can
/// overlap.
#[inline(never)]
fn gather(data: &[u64], seed: &mut u64, every: usize, stop: &dyn Stop) -> Result<u64, StopReason> {
    let mask = data.len() - 1;
    let mut x = *seed;
    let mut sum = 0u64;
    for i in 0..LOADS {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        sum = sum.wrapping_add(data[x as usize & mask]);
        if i & (every - 1) == every - 1 {
            stop.check()?;
        }
    }
    *seed = x;
    Ok(sum)
}

/// `LOADS` dependent loads around a random cycle, checking `stop` every
/// `every` hops: each miss waits for the last.
#[inline(never)]
fn chase(next: &[u32], at: &mut u32, every: usize, stop: &dyn Stop) -> Result<(), StopReason> {
    let mut i = *at;
    for hop in 0..LOADS {
        i = next[i as usize];
        if hop & (every - 1) == every - 1 {
            stop.check()?;
        }
    }
    *at = i;
    Ok(())
}

/// One random cycle through all `n` entries, and three nodes a third of the
/// cycle apart, so three walks of up to `n / 3` hops never share a cache line.
fn random_cycle(n: usize) -> (Vec<u32>, [u32; 3]) {
    let mut order: Vec<u32> = (0..n as u32).collect();
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    for i in (1..n).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        order.swap(i, (x % (i as u64 + 1)) as usize);
    }
    let mut next = vec![0u32; n];
    for k in 0..n {
        next[order[k] as usize] = order[(k + 1) % n];
    }
    (next, [order[0], order[n / 3], order[2 * n / 3]])
}

/// A thread that clones and drops `stop` until told to finish: the `Arc`
/// counts share the flag's cache line, so every check then misses.
struct Contender(Arc<AtomicBool>, std::thread::JoinHandle<()>);

impl Contender {
    fn start<S: Clone + Send + 'static>(stop: &S) -> Self {
        let (stop, done) = (stop.clone(), Arc::new(AtomicBool::new(false)));
        let flag = Arc::clone(&done);
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                drop(black_box(stop.clone()));
            }
        });
        Self(done, thread)
    }
    fn finish(self) {
        self.0.store(true, Ordering::Relaxed);
        self.1.join().unwrap();
    }
}

fn add_gather<S: Stop + Clone + Send + 'static>(
    group: &mut zenbench::BenchGroup,
    name: &str,
    make: fn() -> S,
    data: Arc<Vec<u64>>,
    every: usize,
    contended: bool,
) {
    // Each variant has its own address sequence, carried across rounds, so
    // rounds keep reaching new lines and neither variant warms the other's.
    let mut seed = name.bytes().fold(0x9E37_79B9_7F4A_7C15u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01B3)
    });
    group.bench(name, move |b| {
        let stop = make();
        let contender = contended.then(|| Contender::start(&stop));
        b.iter(|| gather(&data, &mut seed, every, black_box(&stop)));
        if let Some(contender) = contender {
            contender.finish();
        }
    });
}

fn add_chase<S: Stop + 'static>(
    group: &mut zenbench::BenchGroup,
    name: &str,
    make: fn() -> S,
    next: Arc<Vec<u32>>,
    mut at: u32,
    every: usize,
) {
    // The position carries across rounds, so rounds keep reaching new lines.
    group.bench(name, move |b| {
        let stop = make();
        b.iter(|| chase(&next, &mut at, every, black_box(&stop)));
    });
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
            group.baseline("relaxed_flag");
            group.bench("relaxed_flag", |b| {
                let stop = RelaxedFlag::new();
                b.iter(|| check_dyn(black_box(&stop)))
            });
            group.bench("acquire_flag", |b| {
                let stop = AcquireFlag::new();
                b.iter(|| check_dyn(black_box(&stop)))
            });
            group.bench("fenced_flag", |b| {
                let stop = FencedFlag::new();
                b.iter(|| check_dyn(black_box(&stop)))
            });
            group.bench("stopper", |b| {
                let stop = Stopper::new();
                b.iter(|| check_dyn(black_box(&stop)))
            });
        });

        suite.compare("check, generic", |group| {
            group.config().cache_firewall(false);
            group.baseline("relaxed_flag");
            group.bench("relaxed_flag", |b| {
                let stop = RelaxedFlag::new();
                b.iter(|| check_generic(black_box(&stop)))
            });
            group.bench("acquire_flag", |b| {
                let stop = AcquireFlag::new();
                b.iter(|| check_generic(black_box(&stop)))
            });
            group.bench("fenced_flag", |b| {
                let stop = FencedFlag::new();
                b.iter(|| check_generic(black_box(&stop)))
            });
            group.bench("stopper", |b| {
                let stop = Stopper::new();
                b.iter(|| check_generic(black_box(&stop)))
            });
        });

        for chunk in [64, 1024] {
            suite.compare(format!("defilter 64 KiB, check every {chunk} B"), |group| {
                group.config().cache_firewall(false);
                group.baseline("relaxed_flag");
                group.throughput(zenbench::Throughput::Bytes(BUFFER as u64));
                group.bench("relaxed_flag", move |b| {
                    let stop = RelaxedFlag::new();
                    let mut buf = vec![7u8; BUFFER];
                    b.iter(|| defilter(black_box(&mut buf), chunk, black_box(&stop)))
                });
                group.bench("acquire_flag", move |b| {
                    let stop = AcquireFlag::new();
                    let mut buf = vec![7u8; BUFFER];
                    b.iter(|| defilter(black_box(&mut buf), chunk, black_box(&stop)))
                });
                group.bench("fenced_flag", move |b| {
                    let stop = FencedFlag::new();
                    let mut buf = vec![7u8; BUFFER];
                    b.iter(|| defilter(black_box(&mut buf), chunk, black_box(&stop)))
                });
                group.bench("stopper", move |b| {
                    let stop = Stopper::new();
                    let mut buf = vec![7u8; BUFFER];
                    b.iter(|| defilter(black_box(&mut buf), chunk, black_box(&stop)))
                });
            });
        }

        let data: Arc<Vec<u64>> = Arc::new((0..GATHER_ENTRIES as u64).collect());
        for (every, contended) in [(8, false), (64, false), (8, true)] {
            let title = format!(
                "random gather 256 MiB, check every {every} loads{}",
                if contended {
                    ", stop cloned and dropped by another thread"
                } else {
                    ""
                }
            );
            suite.compare(title, |group| {
                group.config().cache_firewall(false);
                group.baseline("relaxed_flag");
                group.throughput(zenbench::Throughput::Elements(LOADS as u64));
                add_gather(
                    group,
                    "relaxed_flag",
                    RelaxedFlag::new,
                    Arc::clone(&data),
                    every,
                    contended,
                );
                add_gather(
                    group,
                    "acquire_flag",
                    AcquireFlag::new,
                    Arc::clone(&data),
                    every,
                    contended,
                );
                add_gather(
                    group,
                    "stopper",
                    Stopper::new,
                    Arc::clone(&data),
                    every,
                    contended,
                );
            });
        }
        drop(data);

        let (next, starts) = random_cycle(CHASE_ENTRIES);
        let next = Arc::new(next);
        for every in [4, 64] {
            suite.compare(
                format!("pointer chase 256 MiB, check every {every} hops"),
                |group| {
                    group.config().cache_firewall(false);
                    group.baseline("relaxed_flag");
                    group.throughput(zenbench::Throughput::Elements(LOADS as u64));
                    add_chase(
                        group,
                        "relaxed_flag",
                        RelaxedFlag::new,
                        Arc::clone(&next),
                        starts[0],
                        every,
                    );
                    add_chase(
                        group,
                        "acquire_flag",
                        AcquireFlag::new,
                        Arc::clone(&next),
                        starts[1],
                        every,
                    );
                    add_chase(
                        group,
                        "stopper",
                        Stopper::new,
                        Arc::clone(&next),
                        starts[2],
                        every,
                    );
                },
            );
        }
    });

    if let Err(e) = result.save("stopper_ordering_results.json") {
        eprintln!("Failed to save results: {e}");
    }
}

//! How long a `cancel()` on one core takes to reach a check on another, for
//! each atomic ordering. One thread stores a sequence number, another spins
//! until it loads it and replies the same way; half a round trip is the time
//! from a store to the load that sees it. The two flags sit on separate
//! 128-byte lines. Pin the process to two CPUs on Linux:
//!
//! ```text
//! taskset -c 0,8 cargo run --release -p almost-enough --example cross_core_latency -- [round_trips] [reps]
//! ```
//!
//! Results: `benchmarks/stopper-ordering-2026-10-06.md`.

use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[repr(align(128))]
struct Line(AtomicU64);

trait Mode: Send + 'static {
    const NAME: &'static str;
    const STORE: Ordering;
    const LOAD: Ordering;
}
struct Relaxed;
impl Mode for Relaxed {
    const NAME: &'static str = "Relaxed";
    const STORE: Ordering = Ordering::Relaxed;
    const LOAD: Ordering = Ordering::Relaxed;
}
struct RelAcq;
impl Mode for RelAcq {
    const NAME: &'static str = "Release/Acquire";
    const STORE: Ordering = Ordering::Release;
    const LOAD: Ordering = Ordering::Acquire;
}
struct SeqCst;
impl Mode for SeqCst {
    const NAME: &'static str = "SeqCst";
    const STORE: Ordering = Ordering::SeqCst;
    const LOAD: Ordering = Ordering::SeqCst;
}

/// Spins without a spin-loop hint: on x86 `pause` adds its own delay to
/// every wait, and the point is to time the store reaching the load.
#[allow(clippy::missing_spin_loop)]
#[inline(never)]
fn wait_for<M: Mode>(line: &Line, value: u64) {
    while line.0.load(M::LOAD) != value {}
}

/// Mean one-way latency in ns over `n` round trips.
fn one_way<M: Mode>(n: u64) -> f64 {
    let ping = Arc::new(Line(AtomicU64::new(0)));
    let pong = Arc::new(Line(AtomicU64::new(0)));
    let (p, q) = (Arc::clone(&ping), Arc::clone(&pong));
    let echo = std::thread::spawn(move || {
        for i in 1..=n + 1 {
            wait_for::<M>(&p, i);
            q.0.store(i, M::STORE);
        }
    });
    // One untimed trip, so both threads are running before the clock starts.
    ping.0.store(1, M::STORE);
    wait_for::<M>(&pong, 1);
    let start = Instant::now();
    for i in 2..=n + 1 {
        ping.0.store(black_box(i), M::STORE);
        wait_for::<M>(&pong, i);
    }
    let elapsed = start.elapsed();
    echo.join().unwrap();
    elapsed.as_nanos() as f64 / n as f64 / 2.0
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn main() {
    let mut args = std::env::args().skip(1);
    let n: u64 = args.next().map_or(200_000, |s| s.parse().unwrap());
    let reps: usize = args.next().map_or(15, |s| s.parse().unwrap());
    let (mut r, mut ra, mut s) = (Vec::new(), Vec::new(), Vec::new());
    // Interleaved, so drift in machine load hits every ordering alike.
    for _ in 0..reps {
        r.push(one_way::<Relaxed>(n));
        ra.push(one_way::<RelAcq>(n));
        s.push(one_way::<SeqCst>(n));
    }
    for (name, v) in [
        (Relaxed::NAME, &mut r),
        (RelAcq::NAME, &mut ra),
        (SeqCst::NAME, &mut s),
    ] {
        let (lo, hi) = (
            v.iter().cloned().fold(f64::MAX, f64::min),
            v.iter().cloned().fold(0.0, f64::max),
        );
        println!(
            "{name:>16}: median {:6.1} ns one-way  (min {:6.1}, max {:6.1}; {reps} reps x {n} round trips)",
            median(v),
            lo,
            hi
        );
    }
}

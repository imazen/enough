//! Does seeing a stop also show the writes made before `cancel()`? A litmus
//! test for a relaxed flag (what `Stopper` was before 0.4.5) against
//! `Stopper` (Release/Acquire).
//!
//! In each round a writer stores a value and then cancels a fresh stop. A
//! reader first reads the value (so its cache holds the old one), then waits
//! until it sees the stop and reads the value again. Seeing the old value
//! after the stop is a stale read. The memory model allows it for the relaxed
//! flag and forbids it for `Stopper`. This counts how often the hardware
//! actually does it. Writer and reader advance in lockstep, so every read
//! races its write.
//!
//! On x86-64 neither can read stale (stores and loads stay in order there).
//! A weakly ordered CPU, such as an ARM core, may reorder either side.
//!
//! ```text
//! cargo run --release -p almost-enough --example stop_ordering -- [MILLION_ROUNDS]
//! ```

use almost_enough::{Stop, StopReason, Stopper};
use std::hint::spin_loop;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Barrier};
use std::time::Instant;

/// A flag with Relaxed ordering on both sides, laid out as `Stopper` was
/// before 0.4.5.
struct RelaxedFlag(Arc<AtomicBool>);

impl RelaxedFlag {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
    fn cancel(&self) {
        self.0.store(true, Relaxed);
    }
}

impl Stop for RelaxedFlag {
    fn check(&self) -> Result<(), StopReason> {
        if self.0.load(Relaxed) {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Rounds per batch; each batch allocates its stops before the race starts.
const BATCH: usize = 1 << 16;

/// Run `rounds` rounds and return how many reads were stale.
fn litmus<S: Stop>(rounds: usize, make: impl Fn() -> S, cancel: impl Fn(&S) + Sync) -> u64 {
    let mut stale = 0;
    for _ in 0..rounds.div_ceil(BATCH) {
        let stops: Vec<S> = (0..BATCH).map(|_| make()).collect();
        let data: Vec<AtomicU32> = (0..BATCH).map(|_| AtomicU32::new(0)).collect();
        let reader_at = AtomicUsize::new(0);
        let start = Barrier::new(2);
        stale += std::thread::scope(|scope| {
            scope.spawn(|| {
                start.wait();
                for (i, (stop, value)) in stops.iter().zip(&data).enumerate() {
                    // Wait until the reader is waiting on this round.
                    while reader_at.load(Relaxed) <= i {
                        spin_loop();
                    }
                    value.store(1, Relaxed);
                    cancel(stop);
                }
            });
            let reader = scope.spawn(|| {
                start.wait();
                let mut stale = 0u64;
                for (i, (stop, value)) in stops.iter().zip(&data).enumerate() {
                    // Hold the old value in this core's cache.
                    std::hint::black_box(value.load(Relaxed));
                    reader_at.store(i + 1, Relaxed);
                    while !stop.should_stop() {
                        spin_loop();
                    }
                    if value.load(Relaxed) == 0 {
                        stale += 1;
                    }
                }
                stale
            });
            reader.join().unwrap()
        });
    }
    stale
}

fn report(name: &str, rounds: usize, run: impl FnOnce() -> u64) {
    let start = Instant::now();
    let stale = run();
    println!(
        "{name}: {stale} stale reads in {rounds} rounds ({:.2e} per round), {:.1} s",
        stale as f64 / rounds as f64,
        start.elapsed().as_secs_f64()
    );
}

fn main() {
    let millions: usize = std::env::args()
        .nth(1)
        .map(|arg| arg.parse().expect("MILLION_ROUNDS"))
        .unwrap_or(10);
    let rounds = millions * 1_000_000;
    println!("{} {}", std::env::consts::ARCH, std::env::consts::OS);
    report("relaxed flag (Stopper before 0.4.5)", rounds, || {
        litmus(rounds, RelaxedFlag::new, RelaxedFlag::cancel)
    });
    report("Stopper (Release/Acquire)", rounds, || {
        litmus(rounds, Stopper::new, Stopper::cancel)
    });
}

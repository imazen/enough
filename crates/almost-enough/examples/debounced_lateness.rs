//! How late does `DebouncedTimeout` stop after checks slow down?
//!
//! Checks back to back for 20 ms, then spins a fixed time per check,
//! with the deadline 50 ms after creation. Prints how many checks it was
//! making per clock read when the checks slowed down, and how long after
//! the deadline the first check returned `TimedOut`, giving up 60 s past it;
//! then the same with `clear_calibration()` called at the slowdown.
//!
//! ```text
//! cargo run --release -p almost-enough --example debounced_lateness
//! ```

use almost_enough::{DebouncedTimeout, Stop, Unstoppable};
use std::time::{Duration, Instant};

fn spin(duration: Duration) {
    let start = Instant::now();
    while start.elapsed() < duration {
        std::hint::spin_loop();
    }
}

fn main() {
    for (clear, check_time) in [false, true].into_iter().flat_map(|clear| {
        [
            Duration::from_micros(10),
            Duration::from_micros(100),
            Duration::from_millis(1),
        ]
        .map(|check_time| (clear, check_time))
    }) {
        let stop = DebouncedTimeout::new(Unstoppable, Duration::from_millis(50));
        // Back-to-back checks, reading the clock only every 10,000.
        let fast_until = Instant::now() + Duration::from_millis(20);
        while Instant::now() < fast_until {
            for _ in 0..10_000 {
                let _ = std::hint::black_box(stop.check());
            }
        }
        let per_read = stop.checks_per_clock_read();
        if clear {
            stop.clear_calibration();
        }
        let label = if clear { ", calibration cleared" } else { "" };
        let give_up = stop.deadline() + Duration::from_secs(60);
        let stopped = loop {
            if stop.check().is_err() {
                break Some(Instant::now());
            }
            if Instant::now() > give_up {
                break None;
            }
            spin(check_time);
        };
        match stopped {
            Some(at) => println!(
                "{check_time:?} per check{label}: {per_read} checks per clock read, stopped {:.1} ms late",
                at.saturating_duration_since(stop.deadline()).as_secs_f64() * 1e3
            ),
            None => println!(
                "{check_time:?} per check{label}: {per_read} checks per clock read, still running 60 s past the deadline"
            ),
        }
    }
}

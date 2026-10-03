//! Reproducible shared-counter stress test; not an encoder throughput estimate.
use how_far_along::{prelude::*, *};
use std::{hint::black_box, time::Instant};
fn measure(threads: usize, independent: bool, batch: u64, units: u64) -> u128 {
    let tree = PulseTree::new(
        Phase::new("bench", Total::Exact(threads as u64 * units)),
        Unstoppable,
    );
    let parts = vec![PhaseSpec::new("worker", 1, Total::Exact(units)); threads];
    let children = if independent {
        tree.split(Execution::ForkJoin, &parts).unwrap()
    } else {
        Vec::new()
    };
    let views: Vec<_> = (0..threads)
        .map(|i| {
            if independent {
                children[i].share().unwrap()
            } else {
                tree.share().unwrap()
            }
        })
        .collect();
    let barrier = std::sync::Barrier::new(threads + 1);
    let elapsed = std::thread::scope(|s| {
        let mut joins = Vec::new();
        for view in &views {
            let barrier = &barrier;
            joins.push(s.spawn(move || {
                let mut pace = view.as_pulse().paced(batch);
                barrier.wait();
                for i in 0..units {
                    black_box(i);
                    if batch == 0 {
                        view.advance(1);
                    } else {
                        pace.step(1).unwrap();
                    }
                }
                if batch != 0 {
                    pace.finish().unwrap();
                }
            }));
        }
        let start = Instant::now();
        barrier.wait();
        for join in joins {
            join.join().unwrap();
        }
        start.elapsed().as_nanos()
    });
    for child in children {
        child.finish(Outcome::Succeeded).unwrap();
    }
    let observer = tree.observer();
    tree.finish(Outcome::Succeeded).unwrap();
    let snap = observer.snapshot();
    if independent {
        assert_eq!(
            snap.children.iter().map(|c| c.completed).sum::<u64>(),
            threads as u64 * units
        );
    } else {
        assert_eq!(snap.completed, threads as u64 * units);
    }
    elapsed
}
fn main() {
    let units = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(250_000);
    println!("threads,layout,batch,units_per_worker,median_ns");
    for threads in [1, 2, 4, 8] {
        for independent in [false, true] {
            for batch in [0, 64, 1024] {
                let mut samples: Vec<_> = (0..3)
                    .map(|_| measure(threads, independent, batch, units))
                    .collect();
                samples.sort();
                println!(
                    "{threads},{},{batch},{units},{}",
                    if independent { "separate" } else { "shared" },
                    samples[1]
                );
            }
        }
    }
}

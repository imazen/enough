//! A library and the application that calls it, in one file.
//!
//! The library depends only on `how-far`: it takes `&dyn Pulse`, plans its
//! stages, checks for cancellation and counts finished work. The application
//! renders progress from another thread and finishes the root when the
//! library returns.
//!
//! Run with `cargo run -p how-far-along --example library`.

mod library {
    use how_far::{PhaseSpec, ProgressExt, Pulse, ResultExt, Stages, StopReason, Total};

    pub fn convert(rows: u64, pulse: &dyn Pulse) -> Result<u64, StopReason> {
        let mut stages = Stages::new(
            pulse,
            &[
                PhaseSpec::new("decode", 2, Total::Exact(rows)).units("rows"),
                PhaseSpec::new("resize", 5, Total::Exact(rows)).units("rows"),
                PhaseSpec::new("encode", 3, Total::Exact(rows)).units("rows"),
            ],
        );
        let result = (|| {
            let mut checksum = 0_u64;
            for stage_cost in [2, 5, 3] {
                stages.run(|stage| {
                    stage.check()?;
                    for row in 0..rows {
                        for i in 0..stage_cost * 20_000 {
                            checksum = checksum.wrapping_mul(31).wrapping_add(row ^ i);
                        }
                        stage.step(1)?;
                    }
                    Ok(())
                })?;
            }
            Ok(checksum)
        })();
        result.finish_phase(stages)
    }
}

use almost_enough::Stopper;
use how_far_along::{Complete, Phase, PulseTree, Snapshot, Total};
use std::{thread, time::Duration};

fn bar(snapshot: &Snapshot) -> String {
    let fraction = snapshot.fraction().unwrap_or(0.0);
    let filled = (fraction * 30.0).round() as usize;
    let current = snapshot
        .children
        .iter()
        .find(|stage| stage.status == how_far_along::Status::Running)
        .map_or("", |stage| stage.name.as_str());
    format!(
        "[{}{}] {:>3.0}% {current}",
        "#".repeat(filled),
        " ".repeat(30 - filled),
        fraction * 100.0
    )
}

fn main() {
    let stop = Stopper::new(); // Call `stop.cancel()` from any thread to stop.
    let tree = PulseTree::new(Phase::new("convert", Total::Unknown), stop.clone());
    let observer = tree.observer();
    let result = thread::scope(|scope| {
        scope.spawn(|| {
            while !observer.is_finished() {
                if let Some(snapshot) = observer.try_snapshot() {
                    eprint!("\r{}", bar(&snapshot));
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        let result = library::convert(400, &tree);
        tree.complete(result)
    });
    eprintln!("\r{}", bar(&observer.snapshot()));
    println!("{result:?}");
}

//! An application-planned tree: serial → parallel → serial, with owned
//! `Phase` handles and scoped threads.
//!
//! Run with `cargo run -p how-far-along --example phases`.

use how_far_along::{Execution, Phase, PhaseSpec, Report, Total};

fn main() -> Result<(), how_far_along::PlanError> {
    let mut job = Phase::new("encode", Total::Unknown);
    let observer = job.observer();
    let [mut before, mut middle, mut after] = job.split(
        Execution::Sequence,
        [
            PhaseSpec::new("prepare", 35, Total::Exact(1)),
            PhaseSpec::new("parallel", 30, Total::Unknown),
            PhaseSpec::new("write", 35, Total::Exact(1)),
        ],
    )?;
    let branches = middle.split_vec(
        Execution::ForkJoin,
        &[
            PhaseSpec::new("small", 1, Total::Exact(2)),
            PhaseSpec::new("large", 1, Total::Exact(20)),
        ],
    )?;
    before.reporter().advance(1);
    before.finish()?;
    std::thread::scope(|scope| {
        for (mut branch, units) in branches.into_iter().zip([2, 20]) {
            scope.spawn(move || {
                let reporter = branch.reporter();
                for _ in 0..units {
                    reporter.advance(1);
                }
                branch.finish().unwrap();
            });
        }
    }); // Every branch has joined here, including the slower one.
    middle.finish()?;
    println!(
        "after the parallel stage: {:.0}%",
        observer.snapshot().fraction().unwrap() * 100.0
    );
    after.reporter().advance(1);
    after.finish()?;
    job.finish()?;
    let done = observer.snapshot();
    println!("{:?}: {:?}", done.status, done.fraction());
    Ok(())
}

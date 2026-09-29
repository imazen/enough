use howfar_along::{Execution, Part, Phase, Report, Total};

fn main() -> Result<(), howfar_along::PlanError> {
    let mut job = Phase::new("encode", Total::Unknown);
    let observer = job.observer();
    let [mut before, mut middle, mut after] = job.split(
        Execution::Sequence,
        [
            Part::new("prepare", 35, Total::Exact(1)),
            Part::new("parallel", 30, Total::Unknown),
            Part::new("write", 35, Total::Exact(1)),
        ],
    )?;
    let branches = middle.split(
        Execution::ForkJoin,
        [
            Part::new("small", 1, Total::Exact(2)),
            Part::new("large", 1, Total::Exact(20)),
        ],
    )?;
    before.progress().advance(1);
    before.finish()?;
    middle.start()?;
    std::thread::scope(|scope| {
        for (mut branch, units) in branches.into_iter().zip([2, 20]) {
            scope.spawn(move || {
                let progress = branch.progress();
                for _ in 0..units {
                    progress.advance(1);
                }
                branch.finish().unwrap();
            });
        }
    }); // All workers have joined here, including the slower branch.
    middle.finish()?;
    println!(
        "after the middle join: {:.0}%",
        observer.snapshot().fraction().unwrap() * 100.0
    );
    after.progress().advance(1);
    after.finish()?;
    job.finish()?;
    println!(
        "{:?}: {:?}",
        observer.snapshot().status,
        observer.snapshot().fraction()
    );
    Ok(())
}

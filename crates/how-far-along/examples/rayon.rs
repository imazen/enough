//! Parallel library work with Rayon: a stage shared by every worker, a
//! fork-join of tiles with their own children, and `'static` tasks that hold
//! owned handles.
//!
//! Run with `cargo run -p how-far-along --example rayon`.

use how_far_along::{
    Execution, Outcome, Phase, PhaseSpec, ProgressExt, Pulse, PulseTree, RunError, Stages,
    StopReason, Total, Unstoppable,
};
use rayon::prelude::*;
use std::{num::NonZeroUsize, sync::mpsc};

fn work(seed: u64) -> u64 {
    (0..20_000).fold(seed, |acc, i| {
        acc.wrapping_mul(6364136223846793005).wrapping_add(i)
    })
}

fn encode(blocks: u64, pulse: &dyn Pulse) -> Result<u64, RunError<StopReason>> {
    let threads = NonZeroUsize::new(rayon::current_num_threads()).unwrap_or(NonZeroUsize::MIN);
    let mut stages = Stages::new(
        pulse,
        &[
            PhaseSpec::new("search", 6, Total::Exact(blocks))
                .units("blocks")
                .execution(Execution::work_pool(threads)),
            PhaseSpec::new("tiles", 3, Total::Unknown),
            PhaseSpec::new("upload", 1, Total::Exact(4)),
        ],
    )?;

    // Every Rayon worker shares one stage and one count.
    let searched = stages.run_stoppable(|stage| {
        (0..blocks)
            .into_par_iter()
            .try_fold(
                || 0_u64,
                |sum, block| {
                    let value = work(block);
                    stage.step(1)?;
                    Ok::<_, StopReason>(sum ^ value)
                },
            )
            .try_reduce(|| 0, |a, b| Ok(a ^ b))
    })?;

    // Each tile is its own child, with its own total and outcome.
    let tiled = stages.run_nested(
        |_| true,
        |stage| {
            let names: Vec<String> = (0..4).map(|i| format!("tile {i}")).collect();
            let parts: Vec<_> = names
                .iter()
                .map(|name| PhaseSpec::new(name, 1, Total::Exact(16)))
                .collect();
            let children = stage.split(Execution::ForkJoin, &parts)?;
            let sums = children
                .into_par_iter()
                .map(|tile| {
                    let result: Result<u64, StopReason> = (0..16).try_fold(0, |sum, i| {
                        tile.step(1)?;
                        Ok(sum ^ work(i))
                    });
                    tile.finish(Outcome::from_result(&result, |_| true))?;
                    result.map_err(RunError::Work)
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(sums.into_iter().fold(0, |a, b| a ^ b))
        },
    )?;

    // `rayon::spawn` needs `'static`, so the tasks own handles, not borrows.
    let uploaded = stages.run_stoppable(|stage| {
        let (done, results) = mpsc::channel();
        for part in 0..4 {
            let handle = stage.handle();
            let done = done.clone();
            rayon::spawn(move || {
                let value = work(part);
                done.send(handle.step(1).map(|()| value)).unwrap();
            });
        }
        drop(done);
        results.iter().try_fold(0, |a, b| b.map(|b| a ^ b))
    })?;
    stages.finish()?;
    Ok(searched ^ tiled ^ uploaded)
}

fn main() {
    let tree = PulseTree::new(Phase::new("encode", Total::Unknown), Unstoppable);
    let observer = tree.observer();
    let result = encode(2_000, &tree);
    tree.finish(Outcome::from_result(&result, |_| true))
        .unwrap();
    let done = observer.snapshot();
    println!("{:?} {:?}", done.status, done.fraction());
    for stage in &done.children {
        println!(
            "  {}: {} {} ({} children)",
            stage.name,
            stage.completed,
            stage.units,
            stage.children.len()
        );
    }
}

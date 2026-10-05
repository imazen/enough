//! Parallel library work with Rayon through ordinary `Result`s: a stage shared
//! by every worker, a fork-join of tiles with their own outcomes, and
//! `'static` tasks that own a shared view of their stage. The library depends
//! only on `how-far`; `main` plays the application and adds a tracker.
//!
//! Run with `cargo run -p how-far-along --example rayon`.

use how_far::{Execution, PhaseSpec, Pulse, SharedPulse, Stages, StopReason, Total, prelude::*};
use how_far_along::{Phase, PulseTree, Unstoppable};
use rayon::prelude::*;
use std::{num::NonZeroUsize, sync::mpsc};

fn work(seed: u64) -> u64 {
    (0..20_000).fold(seed, |acc, i| {
        acc.wrapping_mul(6364136223846793005).wrapping_add(i)
    })
}

fn encode(blocks: u64, pulse: &dyn Pulse) -> Result<u64, StopReason> {
    let threads = NonZeroUsize::new(rayon::current_num_threads()).unwrap_or(NonZeroUsize::MIN);
    Stages::new(
        pulse,
        &[
            PhaseSpec::new("search", 6, Total::Exact(blocks))
                .units("blocks")
                .execution(Execution::work_pool(threads)),
            PhaseSpec::new("tiles", 3, Total::Unknown),
            PhaseSpec::new("upload", 1, Total::Exact(4)),
        ],
    )
    .complete_with(|stages| {
        // Every Rayon worker borrows one stage and adds to one count.
        let searched = stages.run(|stage| {
            (0..blocks)
                .into_par_iter()
                .try_fold(
                    || 0_u64,
                    |sum, block| {
                        let value = work(block);
                        stage.step(1)?;
                        Ok(sum ^ value)
                    },
                )
                .try_reduce(|| 0, |a, b| Ok(a ^ b))
        })?;

        // Each tile is its own child, with its own total and outcome. `plan`
        // never fails the work: a rejected plan still checks cancellation.
        let tiled = stages.run(|stage| {
            let names: Vec<String> = (0..4).map(|i| format!("tile {i}")).collect();
            let parts: Vec<_> = names
                .iter()
                .map(|name| PhaseSpec::new(name, 1, Total::Exact(16)))
                .collect();
            let sums = stage
                .plan(Execution::ForkJoin, &parts)
                .into_par_iter()
                .map(|tile| {
                    let result = (0..16).try_fold(0, |sum, i| {
                        tile.step(1)?;
                        Ok(sum ^ work(i))
                    });
                    tile.complete(result)
                })
                .collect::<Result<Vec<u64>, StopReason>>()?;
            Ok(sums.into_iter().fold(0, |a, b| a ^ b))
        })?;

        // `rayon::spawn` needs `'static`, so those tasks own a shared view.
        // A pulse that only works borrowed refuses to share; scoped tasks then
        // do the same work without giving up its cancellation.
        let uploaded = stages.run(|stage| match stage.share() {
            Ok(shared) => upload_spawned(&shared),
            Err(_) => upload_scoped(stage),
        })?;
        Ok(searched ^ tiled ^ uploaded)
    })
}

fn upload_spawned(stage: &SharedPulse) -> Result<u64, StopReason> {
    let (done, results) = mpsc::channel();
    for part in 0..4 {
        let (stage, done) = (stage.clone(), done.clone());
        rayon::spawn(move || {
            let value = work(part);
            done.send(stage.step(1).map(|()| value)).unwrap();
        });
    }
    drop(done);
    // Every task has sent, so all of them have joined before the stage ends.
    results.iter().try_fold(0, |a, b| b.map(|b| a ^ b))
}

fn upload_scoped(stage: &dyn Pulse) -> Result<u64, StopReason> {
    (0..4_u64)
        .into_par_iter()
        .map(|part| {
            let value = work(part);
            stage.step(1).map(|()| value)
        })
        .try_reduce(|| 0, |a, b| Ok(a ^ b))
}

fn main() {
    let tree = PulseTree::new(Phase::new("encode", Total::Unknown), Unstoppable);
    let observer = tree.observer();
    let result = encode(2_000, &tree).finish_phase(tree);
    let done = observer.snapshot();
    println!("{result:?} {:?} {:?}", done.status, done.fraction());
    for stage in &done.children {
        println!(
            "  {}: {:?} {} {} ({} children)",
            stage.name,
            stage.status,
            stage.completed,
            stage.units,
            stage.children.len()
        );
    }
}

//! What how-far costs for each observer an application passes, crossed with
//! each way a library uses it, around one PNG Sub defilter; counted with
//! `perf stat -e instructions:u,cycles:u` by `measure.py`.
//!
//! usage: how-far-checkpoint-cost OBSERVER SCENARIO CHUNK ITERATIONS
//! (256 KiB buffer; `none` as the observer runs the same work without how-far),
//! or `how-far-checkpoint-cost stop POLICY PATTERN CHUNK ITERATIONS` for the
//! `enough` stop policies alone (see `stop.rs`).
mod stop;

use almost_enough::{FnStop, Stopper};
use how_far::{
    Execution, NoPulse, Outcome, PhaseSpec, Stages, StopOnly, StopReason, Total, Unstoppable,
    WithStop, prelude::*,
};
use how_far_along::{Checkpoint, FnPulse, Phase, PulseTree};
use how_far_really::{diagnostics::DiagnosticPulse, profile::Profiler, profile::StdClock};
use std::hint::black_box;

const BUF: usize = 256 * 1024;
/// Units per reach of a `Paced`, as a library might choose for a 256 KiB row band.
const EVERY: u64 = 64 * 1024;
pub(crate) const WORKERS: usize = 4;

/// Shared by every variant, so they all run the same hot loop at the same
/// address and differ only in how they reach how-far.
#[inline(never)]
pub(crate) fn sub_defilter(buf: &mut [u8]) {
    for i in 4..buf.len() {
        buf[i] = buf[i].wrapping_add(buf[i - 4]);
    }
}

// ── Library scenarios ────────────────────────────────────────────────────────

#[inline(never)]
fn check(buf: &mut [u8], chunk: usize, pulse: &dyn Pulse) -> Result<(), StopReason> {
    for part in buf.chunks_mut(chunk) {
        sub_defilter(part);
        pulse.check()?;
    }
    Ok(())
}

#[inline(never)]
fn step(buf: &mut [u8], chunk: usize, pulse: &dyn Pulse) -> Result<(), StopReason> {
    pulse.check()?;
    for part in buf.chunks_mut(chunk) {
        sub_defilter(part);
        pulse.step(part.len() as u64)?;
    }
    Ok(())
}

#[inline(never)]
fn live(buf: &mut [u8], chunk: usize, pulse: &dyn Pulse) -> Result<(), StopReason> {
    let pulse = pulse.live();
    pulse.check()?;
    for part in buf.chunks_mut(chunk) {
        sub_defilter(part);
        pulse.step(part.len() as u64)?;
    }
    Ok(())
}

#[inline(never)]
fn paced(buf: &mut [u8], chunk: usize, pulse: &dyn Pulse) -> Result<(), StopReason> {
    let mut pace = pulse.paced(EVERY);
    pace.check()?;
    for part in buf.chunks_mut(chunk) {
        sub_defilter(part);
        pace.step(part.len() as u64)?;
    }
    pace.finish()
}

/// One operation of three stages, each paced over a third of the buffer.
#[inline(never)]
fn stages(buf: &mut [u8], chunk: usize, pulse: &dyn Pulse) -> Result<(), StopReason> {
    let third = buf.len() / 3;
    let total = Total::Exact(third as u64);
    Stages::new(
        pulse,
        &[
            PhaseSpec::new("decode", 1, total),
            PhaseSpec::new("filter", 1, total),
            PhaseSpec::new("encode", 1, total),
        ],
    )
    .complete_with(|stages| {
        for part in buf.chunks_mut(third).take(3) {
            stages.run(|stage| paced(part, chunk, stage))?;
        }
        Ok(())
    })
}

/// Four scoped workers share one stage, each on a quarter of the buffer.
#[inline(never)]
fn pool(buf: &mut [u8], chunk: usize, pulse: &dyn Pulse, each: Shape) -> Result<(), StopReason> {
    let quarter = buf.len() / WORKERS;
    std::thread::scope(|scope| {
        let workers: Vec<_> = buf
            .chunks_mut(quarter)
            .map(|part| scope.spawn(move || each(part, chunk, pulse)))
            .collect();
        workers.into_iter().try_for_each(|worker| worker.join().unwrap())
    })
}

/// Four fork-join children, one per worker, each paced over its quarter.
#[inline(never)]
fn fork_join(buf: &mut [u8], chunk: usize, pulse: &dyn Pulse) -> Result<(), StopReason> {
    let quarter = buf.len() / WORKERS;
    let total = Total::Exact(quarter as u64);
    let parts = [PhaseSpec::new("quarter", 1, total); WORKERS];
    let children = pulse.plan(Execution::ForkJoin, &parts);
    std::thread::scope(|scope| {
        let workers: Vec<_> = buf
            .chunks_mut(quarter)
            .zip(children)
            .map(|(part, child)| {
                scope.spawn(move || child.complete_with(|child| paced(part, chunk, child)))
            })
            .collect();
        workers.into_iter().try_for_each(|worker| worker.join().unwrap())
    })
}

/// The same work without how-far: `parallel` splits it across four workers.
#[inline(never)]
fn bare(buf: &mut [u8], chunk: usize, parallel: bool) {
    let work = |part: &mut [u8]| {
        for piece in part.chunks_mut(chunk) {
            sub_defilter(piece);
            black_box(piece.len());
        }
    };
    if parallel {
        let quarter = buf.len() / WORKERS;
        std::thread::scope(|scope| {
            for part in buf.chunks_mut(quarter) {
                scope.spawn(move || work(part));
            }
        });
    } else {
        work(buf);
    }
}

type Shape = fn(&mut [u8], usize, &dyn Pulse) -> Result<(), StopReason>;

/// A scenario's library code, and whether it plans phases, so that an
/// operation needs a fresh observer, as one tracked operation would.
fn scenario(name: &str) -> (Shape, bool) {
    fn pool_step(b: &mut [u8], c: usize, p: &dyn Pulse) -> Result<(), StopReason> {
        pool(b, c, p, step)
    }
    fn pool_paced(b: &mut [u8], c: usize, p: &dyn Pulse) -> Result<(), StopReason> {
        pool(b, c, p, paced)
    }
    match name {
        "check" => (check, false),
        "step" => (step, false),
        "live" => (live, false),
        "paced" => (paced, false),
        "stages" => (stages, true),
        "pool-step" => (pool_step, false),
        "pool-paced" => (pool_paced, false),
        "fork-join" => (fork_join, true),
        _ => panic!("unknown scenario {name}"),
    }
}

// ── Observers ────────────────────────────────────────────────────────────────

/// An `FnPulse` callback as an application installs one, cold and doing
/// nothing, so the numbers are the cost of reaching it.
#[cold]
#[inline(never)]
fn on_checkpoint(progress: &Checkpoint<'_>) -> Result<(), StopReason> {
    black_box(progress);
    Ok(())
}

#[cold]
#[inline(never)]
fn stop_callback() -> bool {
    black_box(false)
}

fn tree(stop: impl Stop + 'static) -> PulseTree {
    PulseTree::new(Phase::new("bench", Total::Unknown), stop)
}

/// Build the named observer, hand it to `work`, then finish it as an
/// application would.
fn observe(name: &str, work: impl FnOnce(&dyn Pulse)) {
    match name {
        "nopulse" => work(&NoPulse),
        "stop-only" => work(&StopOnly::new(Stopper::new())),
        "tree" => {
            let tree = tree(Unstoppable);
            work(&tree);
            tree.finish(Outcome::Succeeded).unwrap();
        }
        "tree-stop" => {
            let tree = tree(Stopper::new());
            work(&tree);
            tree.finish(Outcome::Succeeded).unwrap();
        }
        "tree-stop-callback" => {
            let stop: Box<dyn Fn() -> bool + Send + Sync> = Box::new(stop_callback);
            let tree = tree(FnStop::new(stop));
            work(&tree);
            tree.finish(Outcome::Succeeded).unwrap();
        }
        "callback" => {
            let pulse = FnPulse::new("bench", on_checkpoint);
            work(&pulse);
            pulse.complete_as(Outcome::Succeeded);
        }
        "shared" => {
            let tree = tree(Stopper::new());
            work(&tree.share().unwrap());
            tree.finish(Outcome::Succeeded).unwrap();
        }
        "with-stop" => {
            let (tree, local) = (tree(Stopper::new()), Stopper::new());
            work(&WithStop::borrowed(&tree, &local));
            tree.finish(Outcome::Succeeded).unwrap();
        }
        "diagnostic" => {
            let profiler = Profiler::new(StdClock::new(), 64);
            let pulse = DiagnosticPulse::new(tree(Stopper::new()), &profiler);
            work(&pulse);
            pulse.finish(Outcome::Succeeded).unwrap();
            black_box(profiler.snapshot());
        }
        _ => panic!("unknown observer {name}"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut buf: Vec<u8> = (0..BUF)
        .map(|i| (i.wrapping_mul(0x9E37_79B9) >> 24) as u8)
        .collect();
    if args[1] == "stop" && args[2] != "none" {
        let (chunk, iterations) = (args[4].parse().unwrap(), args[5].parse().unwrap());
        stop::main(&args[2], &args[3], &mut buf, chunk, iterations);
        black_box(&buf);
        return;
    }
    // `stop none PATTERN` is the bare baseline for the stop table.
    let skip = usize::from(args[1] == "stop");
    let (observer, scenario_name) = (args[1 + skip].as_str(), args[2 + skip].as_str());
    let chunk: usize = args[3 + skip].parse().unwrap();
    let iterations: u64 = args[4 + skip].parse().unwrap();
    if observer == "none" {
        let parallel = matches!(scenario_name, "pool" | "pool-step" | "pool-paced" | "fork-join");
        for _ in 0..iterations {
            bare(black_box(&mut buf), black_box(chunk), parallel);
        }
    } else {
        let (work, per_operation) = scenario(scenario_name);
        let mut run = |pulse: &dyn Pulse| {
            black_box(work(black_box(&mut buf), black_box(chunk), black_box(pulse))).unwrap();
        };
        if per_operation {
            for _ in 0..iterations {
                observe(observer, &mut run);
            }
        } else {
            observe(observer, |pulse| {
                for _ in 0..iterations {
                    run(pulse);
                }
            });
        }
    }
    black_box(&buf);
}

//! Checkpoint strategies over one PNG Sub defilter, for
//! `perf stat -e instructions:u,cycles:u`; see `measure.py`.
//!
//! usage: how-far-checkpoint-cost VARIANT CHUNK ITERATIONS (buffer size from
//! `BUF`, 256 KiB by default)
use almost_enough::Stopper;
use how_far::prelude::*;
use how_far::{NoPulse, Outcome, PhaseSpec, RunError, Stages, StopReason};
use how_far_along::{Phase, PulseTree, Total};
use std::hint::black_box;

const BUF: usize = 256 * 1024;
const EVERY: u64 = 64 * 1024;

/// Shared by every variant, so they all run the same hot loop at the same
/// address and differ only in their checkpoint code.
#[inline(never)]
fn sub_defilter(buf: &mut [u8]) {
    for i in 4..buf.len() {
        buf[i] = buf[i].wrapping_add(buf[i - 4]);
    }
}

#[inline(never)]
fn none(buf: &mut [u8], chunk: usize) -> Result<(), StopReason> {
    for part in buf.chunks_mut(chunk) {
        sub_defilter(part);
        black_box(part.len());
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
    Ok(())
}

/// One operation in three stages, each paced over a third of the buffer.
#[inline(never)]
fn operation(buf: &mut [u8], chunk: usize, pulse: &dyn Pulse) -> Result<(), RunError<StopReason>> {
    let third = buf.len() / 3;
    let mut stages = Stages::new(
        pulse,
        &[
            PhaseSpec::new("decode", 1, how_far::Total::Exact(third as u64)),
            PhaseSpec::new("filter", 1, how_far::Total::Exact(third as u64)),
            PhaseSpec::new("encode", 1, how_far::Total::Exact(third as u64)),
        ],
    )?;
    for part in buf.chunks_mut(third).take(3) {
        stages.run_stoppable(|stage| {
            let mut pace = stage.paced(EVERY);
            for piece in part.chunks_mut(chunk) {
                sub_defilter(piece);
                pace.step(piece.len() as u64)?;
            }
            Ok(())
        })?;
    }
    stages.finish()?;
    Ok(())
}

#[inline(never)]
fn operation_none(buf: &mut [u8], chunk: usize) {
    let third = buf.len() / 3;
    for part in buf.chunks_mut(third).take(3) {
        for piece in part.chunks_mut(chunk) {
            sub_defilter(piece);
            black_box(piece.len());
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (variant, chunk, iters) = (args[1].as_str(), args[2].parse().unwrap(), args[3].parse::<u64>().unwrap());
    let size = std::env::var("BUF").map_or(BUF, |b| b.parse().unwrap());
    let mut buf: Vec<u8> = (0..size).map(|i| (i.wrapping_mul(0x9E37_79B9) >> 24) as u8).collect();
    let tree = PulseTree::new(Phase::new("bench", Total::Unknown), Stopper::new());
    let (kind, target) = variant.split_once('-').unwrap_or((variant, ""));
    let pulse: &dyn Pulse = if target == "tree" { &tree } else { &NoPulse };
    for _ in 0..iters {
        let result = match kind {
            "none" => none(black_box(&mut buf), black_box(chunk)),
            "step" => step(black_box(&mut buf), black_box(chunk), black_box(pulse)),
            "live" => live(black_box(&mut buf), black_box(chunk), black_box(pulse)),
            "paced" => paced(black_box(&mut buf), black_box(chunk), black_box(pulse)),
            "op" if target == "none" => {
                operation_none(black_box(&mut buf), black_box(chunk));
                Ok(())
            }
            "op" if target == "nopulse" => operation(black_box(&mut buf), black_box(chunk), &NoPulse)
                .map_err(|_| StopReason::Cancelled),
            "op" => {
                // An application tracks each operation with its own tree.
                let tree = PulseTree::new(Phase::new("job", Total::Unknown), Stopper::new());
                let result = operation(black_box(&mut buf), black_box(chunk), &tree);
                tree.finish(Outcome::from_result(&result, |_| true)).unwrap();
                result.map_err(|_| StopReason::Cancelled)
            }
            _ => panic!("unknown variant {variant}"),
        };
        black_box(result).unwrap();
    }
    black_box(&buf);
}

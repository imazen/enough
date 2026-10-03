//! Checkpoint strategies over one PNG Sub defilter, for
//! `perf stat -e instructions:u,cycles:u`; see `measure.py`.
//!
//! usage: how-far-checkpoint-cost VARIANT CHUNK ITERATIONS (buffer size from
//! `BUF`, 256 KiB by default)
use almost_enough::{FnStop, Stopper};
use how_far::prelude::*;
use how_far::{
    Child, Execution, NoPulse, NoReport, Outcome, PhaseSpec, PlanError, PulseHandle, RunError,
    Stages, StopReason, Unstoppable,
};
use how_far_along::{Checkpoint, FnPulse, Phase, PulseTree, Total};
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

/// Checks for cancellation on every chunk and never reports.
#[inline(never)]
fn check(buf: &mut [u8], chunk: usize, pulse: &dyn Pulse) -> Result<(), StopReason> {
    for part in buf.chunks_mut(chunk) {
        sub_defilter(part);
        pulse.check()?;
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

/// One operation in three stages, each paced over a third of the buffer, or
/// stepped on every chunk when `paced` is false.
#[inline(never)]
fn operation(
    buf: &mut [u8],
    chunk: usize,
    pulse: &dyn Pulse,
    paced: bool,
) -> Result<(), RunError<StopReason>> {
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
            if paced {
                let mut pace = stage.paced(EVERY);
                for piece in part.chunks_mut(chunk) {
                    sub_defilter(piece);
                    pace.step(piece.len() as u64)?;
                }
                pace.finish()?;
            } else {
                for piece in part.chunks_mut(chunk) {
                    sub_defilter(piece);
                    stage.step(piece.len() as u64)?;
                }
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

// ── Report × stop matrix ─────────────────────────────────────────────────────

/// One stop policy and one report sink behind the same `&dyn Pulse` shell, so
/// matrix variants differ only in what the stop and the report do.
struct Combo<S, R> {
    stop: S,
    report: R,
}
impl<S: Stop, R: Report> Stop for Combo<S, R> {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        self.stop.check()
    }
    #[inline]
    fn may_stop(&self) -> bool {
        self.stop.may_stop()
    }
}
impl<S: Stop, R: Report> Report for Combo<S, R> {
    #[inline]
    fn advance(&self, completed: u64) {
        self.report.advance(completed);
    }
    #[inline]
    fn may_report(&self) -> bool {
        self.report.may_report()
    }
}
impl<S: Stop, R: Report> Pulse for Combo<S, R> {
    fn split(&self, _: Execution, _: &[PhaseSpec<'_>]) -> Result<Vec<Child<'_>>, PlanError> {
        Err(PlanError::Unsupported)
    }
    fn handle(&self) -> PulseHandle {
        PulseHandle::default()
    }
}

/// A report callback as an application installs one: boxed, so every report
/// is an indirect call into user code (here a cold function doing nothing).
struct CallbackReport(Box<dyn Fn(u64) + Send + Sync>);
impl Report for CallbackReport {
    fn advance(&self, completed: u64) {
        (self.0)(completed);
    }
}

#[cold]
#[inline(never)]
fn report_callback(completed: u64) {
    black_box(completed);
}

#[cold]
#[inline(never)]
fn stop_callback() -> bool {
    black_box(false)
}

type StopCallback = FnStop<Box<dyn Fn() -> bool + Send + Sync>>;
fn callback_stop() -> StopCallback {
    FnStop::new(Box::new(stop_callback))
}

/// A live counter: the reporter of a tree phase that stays open.
fn counter() -> how_far_along::Reporter {
    let phase = Phase::new("bench", Total::Unknown);
    let reporter = phase.reporter();
    std::mem::forget(phase); // Finishing or dropping it would end the counting.
    reporter
}

fn matrix_pulse(report: &str, stop: &str) -> Box<dyn Pulse> {
    fn with<R: Report + 'static>(report: R, stop: &str) -> Box<dyn Pulse> {
        match stop {
            "none" => Box::new(Combo {
                stop: Unstoppable,
                report,
            }),
            "flag" => Box::new(Combo {
                stop: Stopper::new(),
                report,
            }),
            "call" => Box::new(Combo {
                stop: callback_stop(),
                report,
            }),
            _ => panic!("unknown stop {stop}"),
        }
    }
    match report {
        "none" => with(NoReport, stop),
        "count" => with(counter(), stop),
        "call" => with(CallbackReport(Box::new(report_callback)), stop),
        _ => panic!("unknown report {report}"),
    }
}

/// An `FnPulse` callback as an application installs one, cold and doing
/// nothing, so the numbers are the cost of reaching it.
#[cold]
#[inline(never)]
fn fn_callback(progress: &Checkpoint<'_>) -> Result<(), StopReason> {
    black_box(progress);
    Ok(())
}

fn tree_pulse(stop: &str) -> Box<dyn Pulse> {
    let phase = Phase::new("bench", Total::Unknown);
    match stop {
        "none" => Box::new(PulseTree::new(phase, Unstoppable)),
        "flag" => Box::new(PulseTree::new(phase, Stopper::new())),
        "call" => Box::new(PulseTree::new(phase, callback_stop())),
        _ => panic!("unknown stop {stop}"),
    }
}

fn run_style(
    style: &str,
    buf: &mut [u8],
    chunk: usize,
    pulse: &dyn Pulse,
) -> Result<(), StopReason> {
    match style {
        "step" => step(black_box(buf), black_box(chunk), black_box(pulse)),
        "live" => live(black_box(buf), black_box(chunk), black_box(pulse)),
        "paced" => paced(black_box(buf), black_box(chunk), black_box(pulse)),
        "check" => check(black_box(buf), black_box(chunk), black_box(pulse)),
        _ => panic!("unknown style {style}"),
    }
}

/// `m-STYLE-REPORT-STOP` (the matrix shell), `t-STYLE-STOP` (a `PulseTree`),
/// `n-STYLE` (`&NoPulse`, which `step` recognizes by address), `f-STYLE` (an
/// `FnPulse` with no plan), or `fs-STYLE` (the stage of an `FnPulse` plan with
/// an exact total, as `Stages` hands it out, so each report also moves the
/// fraction).
fn run_matrix(variant: &str, buf: &mut [u8], chunk: usize, iters: u64) -> bool {
    let parts: Vec<&str> = variant.split('-').collect();
    let (style, pulse): (&str, &dyn Pulse) = match parts.as_slice() {
        ["m", style, report, stop] => (*style, Box::leak(matrix_pulse(report, stop))),
        ["t", style, stop] => (*style, Box::leak(tree_pulse(stop))),
        ["n", style] => (*style, &NoPulse),
        ["f", style] => (
            *style,
            Box::leak(Box::new(FnPulse::new("bench", fn_callback))),
        ),
        ["fs", style] => {
            let pulse = FnPulse::new("bench", fn_callback);
            let total = buf.len() as u64 * iters;
            let mut stages =
                Stages::new(&pulse, &[PhaseSpec::new("buffers", 1, Total::Exact(total))]).unwrap();
            stages
                .run_stoppable(|stage| {
                    for _ in 0..iters {
                        run_style(style, buf, chunk, stage)?;
                    }
                    Ok::<(), StopReason>(())
                })
                .unwrap();
            stages.finish().unwrap();
            return true;
        }
        _ => return false,
    };
    for _ in 0..iters {
        black_box(run_style(style, buf, chunk, pulse)).unwrap();
    }
    true
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (variant, chunk, iters) = (
        args[1].as_str(),
        args[2].parse().unwrap(),
        args[3].parse::<u64>().unwrap(),
    );
    let size = std::env::var("BUF").map_or(BUF, |b| b.parse().unwrap());
    let mut buf: Vec<u8> = (0..size)
        .map(|i| (i.wrapping_mul(0x9E37_79B9) >> 24) as u8)
        .collect();
    if run_matrix(variant, &mut buf, chunk, iters) {
        black_box(&buf);
        return;
    }
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
            "op" | "opstep" if target == "nopulse" => operation(
                black_box(&mut buf),
                black_box(chunk),
                &NoPulse,
                kind == "op",
            )
            .map_err(|_| StopReason::Cancelled),
            "op" | "opstep" if target == "fn" => {
                // An application watches each operation with its own callback.
                let pulse = FnPulse::new("bench", fn_callback);
                operation(black_box(&mut buf), black_box(chunk), &pulse, kind == "op")
                    .map_err(|_| StopReason::Cancelled)
            }
            "op" | "opstep" => {
                // An application tracks each operation with its own tree.
                let tree = PulseTree::new(Phase::new("job", Total::Unknown), Stopper::new());
                let result = operation(black_box(&mut buf), black_box(chunk), &tree, kind == "op");
                tree.finish(Outcome::from_result(&result, |_| true))
                    .unwrap();
                result.map_err(|_| StopReason::Cancelled)
            }
            _ => panic!("unknown variant {variant}"),
        };
        black_box(result).unwrap();
    }
    black_box(&buf);
}

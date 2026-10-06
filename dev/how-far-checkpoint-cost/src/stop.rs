//! What each `enough` stop policy costs in each way a library checks it.
//!
//! usage: how-far-checkpoint-cost stop IMPLEMENTATION PATTERN CHUNK ITERATIONS
use crate::{WORKERS, sub_defilter};
use almost_enough::{
    BoxedStop, ChildStopper, DebouncedTimeout, FnStop, OrStop, PollMeter, StopRef, StopSource,
    StopToken, Stopper, SyncStopper, WithTimeout,
};
use enough::{Stop, StopReason, Unstoppable};
use enough_tokio::TokioStop;
use std::{hint::black_box, time::Duration};

/// Check through `&dyn Stop` after every chunk, as a non-generic codec loop does.
#[inline(never)]
fn dynamic(buf: &mut [u8], chunk: usize, stop: &dyn Stop) -> Result<(), StopReason> {
    for part in buf.chunks_mut(chunk) {
        sub_defilter(part);
        stop.check()?;
    }
    Ok(())
}

/// The same loop monomorphized for the concrete policy.
#[inline(never)]
fn generic<S: Stop + ?Sized>(buf: &mut [u8], chunk: usize, stop: &S) -> Result<(), StopReason> {
    for part in buf.chunks_mut(chunk) {
        sub_defilter(part);
        stop.check()?;
    }
    Ok(())
}

#[inline(never)]
fn should_stop(buf: &mut [u8], chunk: usize, stop: &dyn Stop) -> Result<(), StopReason> {
    for part in buf.chunks_mut(chunk) {
        sub_defilter(part);
        if stop.should_stop() {
            return Err(StopReason::Cancelled);
        }
    }
    Ok(())
}

/// Ask `may_stop` once; a policy that can never stop costs a branch per chunk.
#[inline(never)]
fn gated(buf: &mut [u8], chunk: usize, stop: &dyn Stop) -> Result<(), StopReason> {
    let stop = stop.may_stop().then_some(stop);
    for part in buf.chunks_mut(chunk) {
        sub_defilter(part);
        stop.check()?;
    }
    Ok(())
}

/// Check after every 16th chunk.
#[inline(never)]
fn sparse(buf: &mut [u8], chunk: usize, stop: &dyn Stop) -> Result<(), StopReason> {
    for (i, part) in buf.chunks_mut(chunk).enumerate() {
        sub_defilter(part);
        if i % 16 == 15 {
            stop.check()?;
        }
    }
    Ok(())
}

/// Four scoped workers check one shared policy after every chunk.
#[inline(never)]
fn pool(buf: &mut [u8], chunk: usize, stop: &dyn Stop) -> Result<(), StopReason> {
    let quarter = buf.len() / WORKERS;
    std::thread::scope(|scope| {
        let workers: Vec<_> = buf
            .chunks_mut(quarter)
            .map(|part| scope.spawn(move || dynamic(part, chunk, stop)))
            .collect();
        workers.into_iter().try_for_each(|worker| worker.join().unwrap())
    })
}

fn run<S: Stop>(stop: &S, pattern: &str, buf: &mut [u8], chunk: usize, iterations: u64) {
    for _ in 0..iterations {
        let (buf, chunk) = (black_box(&mut *buf), black_box(chunk));
        let result = match pattern {
            "dyn" => dynamic(buf, chunk, black_box(stop)),
            "generic" => generic(buf, chunk, black_box(stop)),
            "should-stop" => should_stop(buf, chunk, black_box(stop)),
            "gated" => gated(buf, chunk, black_box(stop)),
            "sparse" => sparse(buf, chunk, black_box(stop)),
            "pool" => pool(buf, chunk, black_box(stop)),
            _ => panic!("unknown pattern {pattern}"),
        };
        black_box(result).unwrap();
    }
}

#[cold]
#[inline(never)]
fn never() -> bool {
    black_box(false)
}

pub fn main(implementation: &str, pattern: &str, buf: &mut [u8], chunk: usize, iterations: u64) {
    let hour = Duration::from_secs(3600);
    match implementation {
        "unstoppable" => run(&Unstoppable, pattern, buf, chunk, iterations),
        "stopper" => run(&Stopper::new(), pattern, buf, chunk, iterations),
        "sync-stopper" => run(&SyncStopper::new(), pattern, buf, chunk, iterations),
        "stop-ref" => {
            let source = StopSource::new();
            let stop: StopRef<'_> = source.as_ref();
            run(&stop, pattern, buf, chunk, iterations);
        }
        "child" => run(&ChildStopper::new().child().child(), pattern, buf, chunk, iterations),
        "fn-stop" => run(&FnStop::new(never), pattern, buf, chunk, iterations),
        "or" => run(
            &OrStop::new(Stopper::new(), Stopper::new()),
            pattern,
            buf,
            chunk,
            iterations,
        ),
        "timeout" => run(&WithTimeout::new(Stopper::new(), hour), pattern, buf, chunk, iterations),
        "debounced" => run(
            &DebouncedTimeout::new(Stopper::new(), hour),
            pattern,
            buf,
            chunk,
            iterations,
        ),
        "token" => run(&StopToken::new(Stopper::new()), pattern, buf, chunk, iterations),
        "boxed" => run(&BoxedStop::new(Stopper::new()), pattern, buf, chunk, iterations),
        "poll-meter" => run(&PollMeter::new(Stopper::new()), pattern, buf, chunk, iterations),
        "tokio" => run(
            &TokioStop::new(tokio_util::sync::CancellationToken::new()),
            pattern,
            buf,
            chunk,
            iterations,
        ),
        _ => panic!("unknown stop policy {implementation}"),
    }
}

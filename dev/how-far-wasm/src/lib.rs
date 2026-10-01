//! Executable raw-Wasm proof of synchronous polling through a JSPI suspension.
//! Deliberately outside the workspace: normal library compilation builds no host glue.
use almost_enough::Stopper;
use how_far_along::poll::SharedPoller;
use how_far_along::{Outcome, Phase, ProgressExt, ProgressWithStop, Stop, StopReason, Total};

#[link(wasm_import_module = "host")]
unsafe extern "C" {
    fn checkpoint(completed: u32) -> u32;
}

/// Run shared callbacks at every cancellation checkpoint. The host owns this
/// adapter because it puts arbitrary callback work inside the hot loop's checks.
struct PollingStop {
    stop: Stopper,
    poller: SharedPoller,
}

impl Stop for PollingStop {
    fn check(&self) -> Result<(), StopReason> {
        self.stop.check()?;
        self.poller.try_poll();
        self.stop.check()
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn run(units: u32) -> u32 {
    let mut job = Phase::new("wasm", Total::Exact(u64::from(units)));
    let observer = job.observer();
    let stop = Stopper::new();
    let cancel = stop.clone();
    let poller = SharedPoller::new(observer.clone(), move |event| {
        // SAFETY: the host supplies this synchronous import with the declared ABI.
        // JSPI wraps the import and suspends the Wasm stack when it returns a promise.
        if unsafe { checkpoint(event.snapshot().completed as u32) } == 1 {
            cancel.cancel();
        }
    });
    let work = ProgressWithStop::new(PollingStop { stop, poller }, job.reporter());
    let mut outcome = Outcome::Succeeded;
    if work.check().is_err() {
        outcome = Outcome::Cancelled;
    }
    for _ in 0..units {
        if outcome == Outcome::Cancelled {
            break;
        }
        if work.step(1).is_err() {
            outcome = Outcome::Cancelled;
            break;
        }
    }
    job.finish_with(outcome).unwrap();
    observer.snapshot().completed as u32
}

struct Chunked {
    phase: Phase,
    stop: Stopper,
    total: u32,
    completed: u32,
}
thread_local! {
    static CHUNKED: std::cell::RefCell<Option<Chunked>> = const { std::cell::RefCell::new(None) };
}

/// Resumable host adapter for browsers without a stack-suspension mechanism.
#[unsafe(no_mangle)]
pub extern "C" fn begin_chunks(total: u32) {
    CHUNKED.with_borrow_mut(|slot| {
        *slot = Some(Chunked {
            phase: Phase::new("chunks", Total::Exact(u64::from(total))),
            stop: Stopper::new(),
            total,
            completed: 0,
        })
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn run_chunk(budget: u32, cancel: u32) -> u32 {
    CHUNKED.with_borrow_mut(|slot| {
        let job = slot.as_mut().unwrap();
        if job.phase.observer().is_finished() {
            return job.completed;
        }
        if cancel != 0 {
            job.stop.cancel();
        }
        if job.stop.check().is_err() {
            job.phase.finish_with(Outcome::Cancelled).unwrap();
            return job.completed;
        }
        let work = ProgressWithStop::new(&job.stop, job.phase.reporter());
        for _ in 0..budget.min(job.total - job.completed) {
            work.check().unwrap();
            job.completed += 1;
            work.step(1).unwrap();
        }
        if job.completed == job.total {
            job.phase.finish().unwrap();
        }
        job.completed
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn chunks_cancelled() -> u32 {
    CHUNKED.with_borrow(|slot| {
        u32::from(
            slot.as_ref().unwrap().phase.observer().snapshot().status
                == how_far_along::Status::Finished(Outcome::Cancelled),
        )
    })
}

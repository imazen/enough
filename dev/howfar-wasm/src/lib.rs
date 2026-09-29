//! Executable raw-Wasm proof of synchronous polling through a JSPI suspension.
//! Deliberately outside the workspace: normal library compilation builds no host glue.
use howfar_along::poll::{Control, ControlHandle, PollingStop, SharedPoller};
use howfar_along::{Outcome, Phase, Report, Stop, Total, Unstoppable, Work};

#[link(wasm_import_module = "host")]
unsafe extern "C" {
    fn checkpoint(completed: u32) -> u32;
}

#[unsafe(no_mangle)]
pub extern "C" fn run(units: u32) -> u32 {
    let mut job = Phase::new("wasm", Total::Exact(u64::from(units)));
    let observer = job.observer();
    let poller = SharedPoller::new(observer.clone(), ControlHandle::new(), |event| {
        // SAFETY: the host supplies this synchronous import with the declared ABI.
        // JSPI wraps the import and suspends the Wasm stack when it returns a promise.
        let action = unsafe { checkpoint(event.snapshot().completed as u32) };
        if action == 1 {
            Control::Cancel
        } else {
            Control::Continue
        }
    });
    let work = Work::new(PollingStop::new(Unstoppable, poller), job.progress());
    let mut outcome = Outcome::Succeeded;
    for _ in 0..units {
        if work.check().is_err() {
            outcome = Outcome::Cancelled;
            break;
        }
        work.advance(1);
    }
    job.finish_with(outcome).unwrap();
    observer.snapshot().completed as u32
}

struct Chunked {
    phase: Phase,
    control: ControlHandle,
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
            control: ControlHandle::new(),
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
            job.control.cancel();
        }
        if job.control.check().is_err() {
            job.phase.finish_with(Outcome::Cancelled).unwrap();
            return job.completed;
        }
        let work = Work::new(&job.control, job.phase.progress());
        for _ in 0..budget.min(job.total - job.completed) {
            work.check().unwrap();
            work.advance(1);
            job.completed += 1;
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
                == howfar_along::Status::Finished(Outcome::Cancelled),
        )
    })
}

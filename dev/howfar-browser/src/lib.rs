//! Test-only browser host using the same worker/Rayon binding pattern as zenpipe.
use howfar::poll::ControlHandle;
use howfar::{Execution, Observer, Outcome, Part, Phase, Report, Stop, Total};
use rayon::prelude::*;
use std::sync::{Mutex, OnceLock};
use wasm_bindgen::prelude::*;

pub use wasm_bindgen_rayon::init_thread_pool;

struct Job {
    phase: Mutex<Option<Phase>>,
    observer: Observer,
    control: ControlHandle,
}
static JOB: OnceLock<Job> = OnceLock::new();

#[wasm_bindgen]
pub fn prepare() {
    let phase = Phase::new("browser encode", Total::Unknown);
    let observer = phase.observer();
    assert!(
        JOB.set(Job {
            phase: Mutex::new(Some(phase)),
            observer,
            control: ControlHandle::new()
        })
        .is_ok()
    );
}

/// Runs on the owner Worker. Main-thread observer/cancel calls never touch this mutex.
#[wasm_bindgen]
pub fn run(items: u32) -> String {
    let job = JOB.get().unwrap();
    let mut root = job.phase.lock().unwrap().take().unwrap();
    let [mut before, mut middle, mut after] = root
        .split(
            Execution::Sequence,
            [
                Part::new("prepare", 35, Total::Exact(1)),
                Part::new("search", 30, Total::Exact(u64::from(items))).execution(
                    Execution::WorkPool {
                        max_parallelism: rayon::current_num_threads(),
                    },
                ),
                Part::new("write", 35, Total::Exact(1)),
            ],
        )
        .unwrap();
    before.progress().advance(1);
    before.finish().unwrap();
    middle.start().unwrap();
    let progress = middle.progress();
    let result = (0..items).into_par_iter().try_for_each(|item| {
        job.control.check()?;
        // Content-dependent work, counted once per accepted logical item.
        let mut value = u64::from(item);
        for _ in 0..(128 + item % 997) {
            value = std::hint::black_box(value.wrapping_mul(6364136223846793005).wrapping_add(1));
        }
        std::hint::black_box(value);
        progress.advance(1);
        Ok::<(), howfar::StopReason>(())
    }); // Rayon joins all in-flight work before returning.
    if result.is_ok() {
        middle.finish().unwrap();
        after.progress().advance(1);
        after.finish().unwrap();
        root.finish().unwrap();
    } else {
        middle.finish_with(Outcome::Cancelled).unwrap();
        after.finish_with(Outcome::Cancelled).unwrap();
        root.finish_with(Outcome::Cancelled).unwrap();
    }
    observe()
}

/// A real UI-thread read of the shared Rust tree while Rayon workers report.
#[wasm_bindgen]
pub fn observe() -> String {
    let mut json = String::new();
    JOB.get()
        .unwrap()
        .observer
        .snapshot()
        .write_json(&mut json)
        .unwrap();
    json
}

/// A real UI-thread write reaching blocking worker code without a message-loop turn.
#[wasm_bindgen]
pub fn cancel() {
    JOB.get().unwrap().control.cancel();
}

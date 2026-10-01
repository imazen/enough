//! Test-only browser host using the same worker/Rayon binding pattern as zenpipe.
use almost_enough::Stopper;
use how_far_along::profile::{Clock, Profiler, SpanKind};
use how_far_along::{
    Execution, Observer, Outcome, Phase, PhaseSpec, ProgressExt, ProgressWithStop, Report, Stop,
    Total,
};
use std::num::NonZeroUsize;
use rayon::prelude::*;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use wasm_bindgen::prelude::*;

pub use wasm_bindgen_rayon::init_thread_pool;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = performance, js_name = now)]
    fn performance_now() -> f64;
}
// Only the owner Worker records spans here. UI snapshots never call its clock.
struct BrowserClock;
impl Clock for BrowserClock {
    fn now(&self) -> Duration {
        Duration::from_secs_f64(performance_now() / 1000.0)
    }
}

struct Job {
    phase: Mutex<Option<Phase>>,
    observer: Observer,
    stop: Stopper,
    profiler: Profiler,
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
            stop: Stopper::new(),
            profiler: Profiler::new(BrowserClock, 4)
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
                PhaseSpec::new("prepare", 35, Total::Exact(1)),
                PhaseSpec::new("search", 30, Total::Exact(u64::from(items))).execution(
                    Execution::work_pool(
                        NonZeroUsize::new(rayon::current_num_threads()).unwrap_or(NonZeroUsize::MIN),
                    ),
                ),
                PhaseSpec::new("write", 35, Total::Exact(1)),
            ],
        )
        .unwrap();
    before.reporter().advance(1);
    before.finish().unwrap();
    middle.start().unwrap();
    let progress = ProgressWithStop::new(&job.stop, middle.reporter());
    let join = job.profiler.span(middle.id(), "Rayon join", SpanKind::Wait);
    let result = (0..items).into_par_iter().try_for_each(|item| {
        job.stop.check()?;
        // Content-dependent work, counted once per accepted logical item.
        let mut value = u64::from(item);
        for _ in 0..(128 + item % 997) {
            value = std::hint::black_box(value.wrapping_mul(6364136223846793005).wrapping_add(1));
        }
        std::hint::black_box(value);
        progress.step(1)
    }); // Rayon joins all in-flight work before returning.
    if result.is_ok() {
        middle.finish().unwrap();
        after.reporter().advance(1);
        after.finish().unwrap();
        root.finish().unwrap();
    } else {
        middle.finish_with(Outcome::Cancelled).unwrap();
        after.finish_with(Outcome::Skipped).unwrap();
        root.finish_with(Outcome::Cancelled).unwrap();
    }
    join.finish(if result.is_ok() {
        Outcome::Succeeded
    } else {
        Outcome::Cancelled
    });
    job.profiler.operation_returned();
    observe().expect("all writers joined")
}

/// A real UI-thread read of the shared Rust tree while Rayon workers report.
#[wasm_bindgen]
pub fn observe() -> Option<String> {
    let mut json = String::new();
    JOB.get()
        .unwrap()
        .observer
        .try_snapshot()?
        .write_json(&mut json)
        .unwrap();
    Some(json)
}

/// A real UI-thread write reaching blocking worker code without a message-loop turn.
#[wasm_bindgen]
pub fn cancel() {
    JOB.get().unwrap().stop.cancel();
}

/// Optional profiling also uses only a nonblocking read on the UI thread.
#[wasm_bindgen]
pub fn trace() -> Option<String> {
    let mut json = String::new();
    JOB.get()?
        .profiler
        .try_snapshot()?
        .write_json(&mut json)
        .unwrap();
    Some(json)
}

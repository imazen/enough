//! Checkpoint advice for a library, measured without changing its signature.
//!
//! Run with `cargo run -p how-far-along --example diagnostics --features diagnostics`,
//! then replace `library_operation` with the library call you want to test.

use how_far_along::diagnostics::{DiagnosticPulse, Options};
use how_far_along::profile::{Profiler, StdClock};
use how_far_along::{
    Outcome, Phase, PhaseSpec, ProgressExt, Pulse, PulseTree, RunError, Stages, StopReason, Total,
    Unstoppable,
};
use std::{thread, time::Duration};

/// Stand-in for a library: two stages, the second slow and silent.
fn library_operation(pulse: &dyn Pulse) -> Result<(), RunError<StopReason>> {
    let mut stages = Stages::new(
        pulse,
        &[
            PhaseSpec::new("prepare", 50, Total::Exact(1)),
            PhaseSpec::new("encode", 50, Total::Exact(1)),
        ],
    )?;
    for delay in [10, 90] {
        stages.run_stoppable(|stage| {
            stage.check()?;
            thread::sleep(Duration::from_millis(delay));
            stage.step(1)
        })?;
    }
    stages.finish()?;
    Ok(())
}

fn main() {
    let profiler = Profiler::new(StdClock::new(), 16);
    let tree = PulseTree::new(Phase::new("job", Total::Unknown), Unstoppable);
    let measured = DiagnosticPulse::new(tree, &profiler);
    let observer = measured.observer();
    let result = library_operation(&measured);
    measured
        .finish(Outcome::from_result(&result, |_| true))
        .unwrap();
    // Callbacks are measured separately, around each invocation.
    profiler.measure_callback("render", || thread::sleep(Duration::from_millis(12)));
    thread::sleep(Duration::from_millis(15));
    profiler.measure_callback("render", || {});
    let trace = profiler.snapshot().with_progress(observer.snapshot());
    for finding in trace.diagnose(&Options::default()) {
        print!("{finding}");
    }
}

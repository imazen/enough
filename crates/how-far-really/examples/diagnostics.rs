//! Checkpoint advice for a library, measured without changing its signature.
//!
//! Run with `cargo run -p how-far-really --example diagnostics`,
//! then replace `library_operation` with the library call you want to test.

use how_far::{ResultExt, Stages};
use how_far_along::{
    Phase, PhaseSpec, ProgressExt, Pulse, PulseTree, StopReason, Total, Unstoppable,
};
use how_far_really::diagnostics::{DiagnosticPulse, Options};
use how_far_really::profile::{Profiler, StdClock};
use std::{thread, time::Duration};

/// Stand-in for a library: two stages, the second slow and silent.
fn library_operation(pulse: &dyn Pulse) -> Result<(), StopReason> {
    let mut stages = Stages::new(
        pulse,
        &[
            PhaseSpec::new("prepare", 50, Total::Exact(1)),
            PhaseSpec::new("encode", 50, Total::Exact(1)),
        ],
    );
    let result = (|| {
        for delay in [10, 90] {
            stages.run(|stage| {
                stage.check()?;
                thread::sleep(Duration::from_millis(delay));
                stage.step(1)
            })?;
        }
        Ok(())
    })();
    result.finish_phase(stages)
}

fn main() {
    let profiler = Profiler::new(StdClock::new(), 16);
    let tree = PulseTree::new(Phase::new("job", Total::Unknown), Unstoppable);
    let measured = DiagnosticPulse::new(tree, &profiler);
    let observer = measured.observer();
    library_operation(&measured).finish_phase(measured).unwrap();
    // Callbacks are measured separately, around each invocation.
    profiler.measure_callback("render", || thread::sleep(Duration::from_millis(12)));
    thread::sleep(Duration::from_millis(15));
    profiler.measure_callback("render", || {});
    let trace = profiler.snapshot().with_progress(observer.snapshot());
    for finding in trace.diagnose(&Options::default()) {
        print!("{finding}");
    }
}

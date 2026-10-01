//! Run with `cargo run -p how-far-along --example diagnostics --features diagnostics`.
//! Replace `library_operation` with the library call under test.

use how_far::{PhaseSpec, ProgressExt, Pulse, Steps, StopReason};
use how_far_along::diagnostics::{DiagnosticPulse, Options};
use how_far_along::profile::{Profiler, StdClock};
use how_far_along::{Outcome, Phase, PulseTree, Total, Unstoppable};
use std::{thread, time::Duration};

fn library_operation(pulse: &dyn Pulse) {
    let mut stages = Steps::new(
        pulse,
        &[
            PhaseSpec::new("prepare", 50, Total::Exact(1)),
            PhaseSpec::new("encode", 50, Total::Exact(1)),
        ],
    )
    .unwrap();
    for delay in [10, 90] {
        stages
            .run_stoppable(|stage| {
                stage.check()?;
                thread::sleep(Duration::from_millis(delay));
                stage.step(1)?;
                Ok::<(), StopReason>(())
            })
            .unwrap();
    }
    stages.finish().unwrap();
}

fn main() {
    let profiler = Profiler::new(StdClock::new(), 16);
    let stop = Unstoppable;
    let pulse = PulseTree::new(Phase::new("job", Total::Unknown), &stop);
    let observer = pulse.observer();
    let measured = DiagnosticPulse::new(&pulse, observer.clone(), &profiler);
    library_operation(&measured);
    profiler.measure_callback(0, "render", || thread::sleep(Duration::from_millis(12)));
    thread::sleep(Duration::from_millis(15));
    profiler.measure_callback(0, "render", || {});
    let trace = profiler.snapshot().with_progress(observer.snapshot());
    assert_eq!(
        trace.progress.as_ref().unwrap().status,
        how_far_along::Status::Finished(Outcome::Succeeded)
    );
    for finding in trace.diagnose(&Options::default()) {
        print!("{finding}");
    }
}

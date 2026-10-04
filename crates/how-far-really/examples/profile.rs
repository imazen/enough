//! Four tasks sharing one counter, each measured in its own span.
//!
//! Run with `cargo run -p how-far-really --example profile`.

use how_far::ProgressWithStop;
use how_far_along::{Outcome, Phase, ProgressExt, Stop, Total, Unstoppable};
use how_far_really::profile::{Profiler, SpanKind, StdClock};

fn main() {
    let profiler = Profiler::new(StdClock::new(), 64);
    profiler.metadata("run", "example");
    profiler.metadata("configuration", "four independent tasks");
    let mut job = Phase::new("pool", Total::Exact(400));
    let reporter = job.reporter();
    std::thread::scope(|scope| {
        for task in 0..4 {
            let profiler = &profiler;
            let reporter = reporter.clone();
            let node = job.id();
            scope.spawn(move || {
                let span = profiler.span(node, format!("chunk-{task}"), SpanKind::Work);
                let work = span.instrument(ProgressWithStop::new(Unstoppable, reporter));
                work.check().unwrap();
                for _ in 0..100 {
                    work.step(1).unwrap();
                }
                span.finish(Outcome::Succeeded);
            });
        }
    });
    job.finish().unwrap();
    profiler.operation_returned();
    let trace = profiler.snapshot().with_progress(job.observer().snapshot());
    println!("{trace}");
    let mut json = String::new();
    trace.write_json(&mut json).unwrap();
    println!("{json}");
}

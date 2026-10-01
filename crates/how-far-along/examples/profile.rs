use how_far_along::profile::{Profiler, SpanKind, StdClock};
use how_far_along::{Outcome, Phase, ProgressExt, ProgressWithStop, Stop, Total, Unstoppable};

fn main() {
    let profiler = Profiler::new(StdClock::new(), 64);
    profiler.metadata("run", "example");
    profiler.metadata("configuration", "four independent tasks");
    let mut job = Phase::new("pool", Total::Exact(400));
    let progress = job.progress();
    std::thread::scope(|scope| {
        for task in 0..4 {
            let profiler = &profiler;
            let progress = progress.clone();
            scope.spawn(move || {
                let span = profiler.span(0, format!("chunk-{task}"), SpanKind::Work);
                let work = span.instrument(ProgressWithStop::new(Unstoppable, progress));
                for _ in 0..100 {
                    work.check().unwrap();
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

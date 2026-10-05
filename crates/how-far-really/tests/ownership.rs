use how_far_along::{prelude::*, *};
fn tree(total: Total) -> PulseTree {
    PulseTree::new(Phase::new("job", total), Unstoppable)
}
#[test]
fn diagnostic_shared_handles_preserve_nested_spans_and_do_not_own_completion() {
    use how_far_really::{
        diagnostics::DiagnosticPulse,
        profile::{Profiler, StdClock},
    };
    let profiler = Profiler::new(StdClock::new(), 32);
    let measured = DiagnosticPulse::new(tree(Total::Unknown), &profiler);
    let observer = measured.observer();
    let shared = measured.share().unwrap();
    let worker = shared.clone();
    std::thread::spawn(move || leaf(&worker, 33).unwrap())
        .join()
        .unwrap();
    measured.finish(Outcome::Succeeded).unwrap();
    let before = profiler.snapshot();
    shared.advance(100);
    shared.check().unwrap();
    let after = profiler.snapshot();
    assert_eq!(after.spans.len(), before.spans.len());
    assert_eq!(after.active_spans, 0);
    assert!(after.spans.iter().any(|s| s.task == "leaf"));
    assert_eq!(observer.snapshot().children[0].completed, 33);
}

fn leaf(p: &dyn Pulse, n: u64) -> Result<(), StopReason> {
    let mut stages = how_far::Stages::new(p, &[PhaseSpec::new("leaf", 1, Total::Exact(n))]);
    let result = stages.run(|s| {
        s.check()?;
        let mut paced = s.paced(17);
        for _ in 0..n {
            paced.step(1)?;
        }
        paced.finish()
    });
    result.finish_phase(stages)
}

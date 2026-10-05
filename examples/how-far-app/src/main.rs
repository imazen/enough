use how_far::{Outcome, Unstoppable};
use how_far_along::{Snapshot, Status};
use how_far_example_app::observe;
use how_far_example_codec::Mode;
use how_far_example_pipeline::{self as pipeline, Bug};
use how_far_really::diagnostics::Options;

fn tree(node: &Snapshot, depth: usize) {
    println!(
        "{}{}: {:?}{}",
        "  ".repeat(depth),
        node.name,
        node.status,
        if node.completion_inferred {
            " (inferred)"
        } else {
            ""
        }
    );
    for child in &node.children {
        tree(child, depth + 1);
    }
}
fn show(
    name: &str,
    result: &impl std::fmt::Debug,
    mut trace: how_far_really::profile::Trace,
    json: bool,
) {
    if json {
        trace.metadata.push(("scenario".into(), name.into()));
        let mut document = String::new();
        trace.write_json(&mut document).unwrap();
        println!("{document}");
    } else {
        println!("\n{name}: {result:?}");
        tree(trace.progress.as_ref().unwrap(), 0);
        for finding in trace.diagnose(&Options::default()) {
            println!("  {finding}");
        }
    }
}
fn main() {
    let json = std::env::args().any(|arg| arg == "--json");
    for (name, mode) in [
        ("success", Mode::Fast),
        ("recovery", Mode::Fallback),
        ("fatal", Mode::Fatal),
        ("fallback fails", Mode::FallbackFails),
    ] {
        let (result, trace) = observe(Unstoppable, |p| pipeline::convert(&[1, 2], p, mode, false));
        show(name, &result, trace, json);
    }
    for bug in [
        Bug::MissingHandoff,
        Bug::EarlyQuestionMark,
        Bug::MissingReports,
        Bug::DuplicateStage,
        Bug::CountBeforePlan,
        Bug::ReportToBranch,
        Bug::ForgottenRequiredStage,
    ] {
        let (result, trace) = observe(Unstoppable, |p| pipeline::buggy(p, bug));
        show(&format!("{bug:?}"), &result, trace, json);
    }
    // A successful return can conceal missing required work. The observer can
    // show inferred completion, but only the library can assert its obligations.
    let (_, trace) = observe(Unstoppable, |p| {
        pipeline::buggy(p, Bug::ForgottenRequiredStage)
    });
    assert_eq!(
        trace.progress.unwrap().status,
        Status::Finished(Outcome::Succeeded)
    );
}

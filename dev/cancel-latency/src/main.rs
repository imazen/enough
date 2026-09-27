//! Cross-codec adversarial cancellation-latency harness.
//!
//! Runs each enabled codec through a [`PollMeter`]-wrapped cancellation token
//! on deliberately expensive inputs and prints the inter-poll gap report.
//! Slow gaps (>= 50 ms) and poll storms (>= 1M sub-0.5 ms calls) are flagged
//! by the report itself.
//!
//! Usage:
//!   cancel-latency             — run all cases
//!   cancel-latency list        — list cases
//!   cancel-latency <name>...   — run named cases only
//!   cancel-latency --histogram — force ASCII histogram for every case

mod cases;
mod inputs;

use almost_enough::PollMeter;
use enough::Unstoppable;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let histogram = args.iter().any(|a| a == "--histogram");
    let names: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .map(String::as_str)
        .collect();

    let registry = cases::all();
    if names.first().is_some_and(|n| *n == "list") {
        for c in &registry {
            println!("{:32} [{}] {}", c.name, c.codec, c.blurb);
        }
        return;
    }

    let selected: Vec<&cases::Case> = if names.is_empty() {
        registry.iter().collect()
    } else {
        names
            .iter()
            .filter_map(|n| {
                let c = registry.iter().find(|c| c.name == *n);
                if c.is_none() {
                    eprintln!("unknown case '{n}' (run `list`)");
                }
                c
            })
            .collect()
    };

    if selected.is_empty() {
        eprintln!("no cases selected");
        std::process::exit(2);
    }

    let mut any_problems = false;
    for case in selected {
        println!("=== {} [{}] ===", case.name, case.codec);
        println!("  {}", case.blurb);
        let meter = PollMeter::new(Unstoppable);
        let t = std::time::Instant::now();
        match (case.run)(&meter) {
            Ok(msg) => println!("  ok: {msg}"),
            Err(e) => println!("  FAILED: {e}"),
        }
        let wall = t.elapsed();
        let report = meter.report();
        // A report can't flag a gap that never closes: an operation that
        // polls fewer than twice over >> 50ms is cancellation-blind, not clean.
        if report.calls < 2 && wall > std::time::Duration::from_millis(50) {
            any_problems = true;
            println!(
                "  SILENT: {wall:?} of work with only {} poll(s) — no observable cancellation",
                report.calls
            );
        }
        if !report.problems().is_empty() {
            any_problems = true;
        }
        if histogram {
            println!("{report:#}");
        } else {
            println!("{report}");
        }
        println!("  wall: {wall:?}");
        println!();
    }
    if any_problems {
        std::process::exit(1);
    }
}

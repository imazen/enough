//! Diagnostics across crate boundaries: a test measures libraries it does not
//! own, including checks made inside a codec context that owns its stop.

use how_far_along::{NodeId, Outcome, Unstoppable};
use how_far_really::diagnostics::{DiagnosticPulse, Kind, Options};
use how_far_really::profile::{Profiler, SpanKind, StdClock};
use test_how_far_app::{find, images, tree};
use test_how_far_pipeline::{PipelineError, process_each};

#[test]
fn each_codec_stage_in_each_image_gets_its_own_span() {
    let batch = images(2);
    let profiler = Profiler::new(StdClock::new(), 256);
    let measured = DiagnosticPulse::new(tree("batch", Unstoppable), &profiler);
    let observer = measured.observer();
    let result = process_each(&batch, &measured);
    measured
        .finish(Outcome::from_result(&result, PipelineError::is_stop))
        .unwrap();
    let root = observer.snapshot();
    let trace = profiler.snapshot().with_progress(root.clone());
    assert_eq!(trace.dropped_spans, 0);
    assert_eq!(trace.active_spans, 0);
    for (i, image) in batch.iter().enumerate() {
        let image_name = format!("image {i}");
        for stage in ["analyze", "transform", "entropy"] {
            let node: NodeId = find(&root, &[&image_name, stage]).id;
            let span = trace
                .spans
                .iter()
                .find(|span| span.node == Some(node))
                .unwrap_or_else(|| panic!("no span for {image_name}/{stage}"));
            assert_eq!(span.kind, SpanKind::Work);
            assert_eq!(span.outcome, Outcome::Succeeded);
            if stage == "entropy" {
                // The coder's own checks, made through its stored handle,
                // land in this stage's span: one per 64 bytes plus the steps.
                let bytes = image.pixels.len() as u64;
                assert!(span.stats.checks >= bytes / 64, "{}", span.stats.checks);
                assert_eq!(span.stats.units, image.tiles() as u64);
            }
        }
    }
    let findings = trace.diagnose(&Options::default());
    assert!(
        !findings.iter().any(|f| f.kind == Kind::IncompleteEvidence),
        "{findings:#?}"
    );
}

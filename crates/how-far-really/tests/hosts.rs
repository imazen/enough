//! Host-level contracts: real CPU pools, joins, a CLI renderer, and an async
//! request owner.
use almost_enough::Stopper;
use how_far::ProgressWithStop;
use how_far_along::poll::{LocalPoller, SharedPoller};
use how_far_along::{
    Execution, Outcome, Phase, PhaseSpec, ProgressExt, Report, Status, Stop, Total,
};
use how_far_really::profile::{Profiler, SpanKind, StdClock};
use rayon::prelude::*;
use std::{cell::RefCell, fmt::Write, num::NonZeroUsize, rc::Rc, sync::Arc};

fn search_block(block: usize) -> u64 {
    let mut value = block as u64;
    for _ in 0..(1 + block % 23) {
        value = value.wrapping_mul(6364136223846793005).wrapping_add(1);
    }
    value
}

#[test]
fn codec_pipeline_counts_accepted_blocks_across_two_parallel_waves_and_a_serial_filter() {
    for cancel in [false, true] {
        let (width, height, sb) = (257_usize, 193_usize, 64_usize);
        let blocks = width.div_ceil(sb) * height.div_ceil(sb);
        let pool_of_4 = Execution::work_pool(NonZeroUsize::new(4).unwrap());
        let mut job = Phase::new("encode", Total::Unknown);
        let observer = job.observer();
        let [mut prepare, mut search, mut filter, mut pack] = job
            .split(
                Execution::Sequence,
                [
                    PhaseSpec::new("resize", 5, Total::Exact(17)).units("rows"),
                    PhaseSpec::new("search", 82, Total::Exact(blocks as u64))
                        .units("superblocks")
                        .execution(pool_of_4),
                    PhaseSpec::new("filter", 8, Total::Unknown),
                    PhaseSpec::new("pack", 5, Total::Exact(blocks as u64))
                        .units("superblocks")
                        .execution(pool_of_4),
                ],
            )
            .unwrap();
        let profiler = Profiler::new(StdClock::new(), blocks * 2 + 4);
        profiler.metadata("geometry", format!("{width}x{height}; sb={sb}"));
        let stop = Stopper::new();
        let search_observer = search.observer();
        let request_profile = profiler.clone();
        let cancel_after_three = stop.clone();
        let poller = SharedPoller::new(search_observer.clone(), move |event| {
            if cancel && event.snapshot().completed >= 3 && !cancel_after_three.is_cancelled() {
                request_profile.cancellation_requested();
                cancel_after_three.cancel();
            }
        });
        // Strided preparation includes the short final row batch.
        let input = [0_u8; 17];
        for chunk in input.chunks(16) {
            prepare.reporter().advance(chunk.len() as u64);
        }
        prepare.finish().unwrap();
        let workers = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        let reporter = search.reporter();
        let results = workers.install(|| {
            (0..blocks)
                .into_par_iter()
                .map(|block| {
                    let span = profiler.span(search.id(), format!("sb-{block}"), SpanKind::Work);
                    let work =
                        span.instrument(ProgressWithStop::new(stop.clone(), reporter.clone()));
                    let result = (|| {
                        // Fine checkpoints do not claim accepted superblocks.
                        for _ in 0..(1 + block % 23) {
                            work.check()?;
                        }
                        let result = search_block(block);
                        work.step(1)?;
                        poller.try_poll();
                        Ok::<_, how_far_along::StopReason>(result)
                    })();
                    span.finish(if result.is_ok() {
                        Outcome::Succeeded
                    } else {
                        Outcome::Cancelled
                    });
                    result
                })
                .collect::<Vec<_>>()
        }); // `collect` joins every branch, including cancelled ones.
        if cancel {
            assert!(results.iter().any(Result::is_err));
            search.finish_with(Outcome::Cancelled).unwrap();
            filter.finish_with(Outcome::Skipped).unwrap();
            pack.finish_with(Outcome::Skipped).unwrap();
            job.finish_with(Outcome::Cancelled).unwrap();
            assert!(search_observer.snapshot().completed >= 3);
            // In-flight branches may finish their unit before seeing the stop.
            assert!(search_observer.snapshot().completed <= blocks as u64);
        } else {
            search.finish().unwrap();
            let filtered: Vec<_> = results
                .into_iter()
                .map(|result| result.unwrap().rotate_left(3))
                .collect();
            filter
                .set_total(Total::Exact(filtered.len() as u64))
                .unwrap();
            filter.reporter().advance(filtered.len() as u64);
            filter.finish().unwrap();
            let reporter = pack.reporter();
            let output = workers.install(|| {
                filtered
                    .par_iter()
                    .enumerate()
                    .map(|(block, value)| {
                        let span =
                            profiler.span(pack.id(), format!("pack-{block}"), SpanKind::Work);
                        let work = span.instrument(ProgressWithStop::new(&stop, &reporter));
                        let bytes = value.to_le_bytes();
                        work.step(1).unwrap();
                        span.finish(Outcome::Succeeded);
                        bytes
                    })
                    .collect::<Vec<_>>()
            });
            let expected: Vec<_> = (0..blocks)
                .map(|block| search_block(block).rotate_left(3).to_le_bytes())
                .collect();
            assert_eq!(output, expected);
            pack.finish().unwrap();
            job.finish().unwrap();
        }
        profiler.operation_returned();
        let snapshot = observer.snapshot();
        assert_eq!(snapshot.children[0].completed, 17);
        assert_eq!(
            snapshot.children[3].completed,
            if cancel { 0 } else { blocks as u64 }
        );
        let trace = profiler.snapshot().with_progress(snapshot);
        assert_eq!(trace.active_spans, 0);
        assert_eq!(trace.dropped_spans, 0);
        let reported: u64 = trace.spans.iter().map(|s| s.stats.units).sum();
        let tree = trace.progress.as_ref().unwrap();
        assert_eq!(
            reported,
            tree.children[1].completed + tree.children[3].completed
        );
        assert!(trace.spans.iter().map(|s| s.stats.checks).sum::<u64>() > reported);
        if cancel {
            assert!(trace.cancellation_return_latency().is_some());
        }
    }
}

#[test]
fn a_console_renderer_samples_lazily_and_receives_the_final_state() {
    let mut job = Phase::new("download", Total::Exact(17));
    let output = Rc::new(RefCell::new(String::new()));
    let display = output.clone();
    let mut poller = LocalPoller::new(job.observer());
    poller.subscribe(move |event| {
        let snapshot = event.snapshot();
        writeln!(
            display.borrow_mut(),
            "{}: {} units; {:?}",
            snapshot.name,
            snapshot.completed,
            snapshot.status
        )
        .unwrap();
    });
    for size in [16, 1] {
        job.reporter().advance(size);
        poller.poll();
    }
    job.finish().unwrap();
    poller.poll();
    assert!(
        output
            .borrow()
            .contains("download: 17 units; Finished(Succeeded)")
    );
}

#[test]
fn a_server_disconnect_cancels_blocking_work_and_joins_before_returning() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut job = Phase::new("request", Total::Unknown);
        let observer = job.observer();
        let stop = Stopper::new();
        let profiler = Profiler::new(StdClock::new(), 1);
        let (connected, disconnected) = tokio::sync::oneshot::channel::<()>();
        let (started, started_rx) = tokio::sync::oneshot::channel();
        let worker_stop = stop.clone();
        let trace = profiler.clone();
        let worker = tokio::task::spawn_blocking(move || {
            let span = trace.span(job.id(), "CPU request", SpanKind::Work);
            let work = span.instrument(ProgressWithStop::new(worker_stop, job.reporter()));
            started.send(()).unwrap();
            while work.check().is_ok() {
                work.advance(1);
                std::thread::yield_now();
            }
            job.finish_with(Outcome::Cancelled).unwrap();
            span.finish(Outcome::Cancelled);
        });
        let cancel = stop.clone();
        let trace = profiler.clone();
        let disconnect_task = tokio::spawn(async move {
            if disconnected.await.is_err() {
                trace.cancellation_requested();
                cancel.cancel();
            }
        });
        started_rx.await.unwrap();
        drop(connected); // The client goes away while the CPU work runs.
        disconnect_task.await.unwrap();
        worker.await.unwrap(); // The request returns only after the worker joins.
        profiler.operation_returned();
        assert_eq!(
            observer.snapshot().status,
            Status::Finished(Outcome::Cancelled)
        );
        let trace = Arc::new(profiler.snapshot());
        assert!(trace.cancellation_observation_latency().is_some());
        assert!(trace.cancellation_return_latency() >= trace.cancellation_observation_latency());
        assert_eq!(trace.active_spans, 0);
    });
}

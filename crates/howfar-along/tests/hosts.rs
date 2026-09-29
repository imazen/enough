#![cfg(all(feature = "std", feature = "profile"))]
//! Host-level contracts: actual CPU pools, joins, a CLI renderer, and an async request owner.
use howfar_along::ext::WorkExt;
use howfar_along::poll::{Control, ControlHandle, LocalPoller, PollingStop, SharedPoller};
use howfar_along::profile::{Profiler, SpanKind, StdClock};
use howfar_along::{
    Execution, Outcome, Part, Phase, Report, Status, Stop, Total, Unstoppable, Work,
};
use rayon::prelude::*;
use std::{cell::RefCell, fmt::Write, rc::Rc, sync::Arc};

fn search_block(block: usize) -> u64 {
    let mut value = block as u64;
    for _ in 0..(1 + block % 23) {
        value = value.wrapping_mul(6364136223846793005).wrapping_add(1);
    }
    value
}

#[test]
fn codec_pipeline_counts_accepted_blocks_across_two_parallel_waves_and_serial_filter() {
    for cancel in [false, true] {
        let (width, height, sb) = (257_usize, 193_usize, 64_usize);
        let blocks = width.div_ceil(sb) * height.div_ceil(sb);
        let mut job = Phase::new("encode", Total::Unknown);
        let observer = job.observer();
        let [mut prepare, mut search, mut filter, mut pack] = job
            .split(
                Execution::Sequence,
                [
                    Part::new("resize", 5, Total::Exact(17)).units("rows"),
                    Part::new("search", 82, Total::Exact(blocks as u64))
                        .units("superblocks")
                        .execution(Execution::WorkPool { max_parallelism: 4 }),
                    Part::new("filter", 8, Total::Unknown),
                    Part::new("pack", 5, Total::Exact(blocks as u64))
                        .units("superblocks")
                        .execution(Execution::WorkPool { max_parallelism: 4 }),
                ],
            )
            .unwrap();
        let profiler = Profiler::new(StdClock::new(), blocks * 2 + 4);
        profiler.metadata("geometry", format!("{width}x{height}; sb={sb}"));
        let control = ControlHandle::new();
        let search_observer = search.observer();
        let request_profile = profiler.clone();
        let hook = SharedPoller::new(search_observer.clone(), control.clone(), move |event| {
            if cancel && event.snapshot().completed >= 3 {
                request_profile.cancellation_requested();
                Control::Cancel
            } else {
                Control::Continue
            }
        });
        // Strided preparation includes the partial final row batch.
        let input = [0_u8; 17];
        for chunk in input.chunks(16) {
            prepare.progress().advance(chunk.len() as u64);
        }
        prepare.finish().unwrap();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        let report = search.progress();
        let results = pool.install(|| {
            (0..blocks)
                .into_par_iter()
                .map(|block| {
                    let span = profiler.span(search.id(), format!("sb-{block}"), SpanKind::Work);
                    let work = span.instrument(Work::new(
                        PollingStop::new(Unstoppable, hook.clone()),
                        report.clone(),
                    ));
                    let result = (|| {
                        // Fine cancellation checkpoints do not claim accepted superblocks.
                        for _ in 0..(1 + block % 23) {
                            work.check()?;
                        }
                        let result = search_block(block);
                        work.step(1)?;
                        Ok::<_, howfar_along::StopReason>(result)
                    })();
                    span.finish(if result.is_ok() {
                        Outcome::Succeeded
                    } else {
                        Outcome::Cancelled
                    });
                    result
                })
                .collect::<Vec<_>>()
        }); // Collect joins every branch, including cancellation tails.
        if cancel {
            assert!(results.iter().any(Result::is_err));
            search.finish_with(Outcome::Cancelled).unwrap();
            filter.finish_with(Outcome::Cancelled).unwrap();
            pack.finish_with(Outcome::Cancelled).unwrap();
            job.finish_with(Outcome::Cancelled).unwrap();
            assert!(search_observer.snapshot().completed >= 3);
            // In-flight branches can finish their units before observing cancel.
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
            filter.progress().advance(filtered.len() as u64);
            filter.finish().unwrap();
            let report = pack.progress();
            let output = pool.install(|| {
                filtered
                    .par_iter()
                    .enumerate()
                    .map(|(block, value)| {
                        let span =
                            profiler.span(pack.id(), format!("pack-{block}"), SpanKind::Work);
                        let work = span.instrument(Work::new(&control, &report));
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
fn console_renderer_samples_lazily_and_receives_terminal_output() {
    let mut job = Phase::new("download", Total::Exact(17));
    let output = Rc::new(RefCell::new(String::new()));
    let display = output.clone();
    let mut poller = LocalPoller::new(job.observer(), ControlHandle::new());
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
        Control::Continue
    });
    for size in [16, 1] {
        job.progress().advance(size);
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
fn server_disconnect_cancels_blocking_work_and_joins_before_returning() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut job = Phase::new("request", Total::Unknown);
        let observer = job.observer();
        let control = ControlHandle::new();
        let profiler = Profiler::new(StdClock::new(), 1);
        let (connected, disconnected) = tokio::sync::oneshot::channel::<()>();
        let (started, started_rx) = tokio::sync::oneshot::channel();
        let stop = control.clone();
        let trace = profiler.clone();
        let worker = tokio::task::spawn_blocking(move || {
            let span = trace.span(job.id(), "CPU request", SpanKind::Work);
            let work = span.instrument(Work::new(stop, job.progress()));
            started.send(()).unwrap();
            while work.check().is_ok() {
                work.advance(1);
                std::thread::yield_now();
            }
            job.finish_with(Outcome::Cancelled).unwrap();
            span.finish(Outcome::Cancelled);
        });
        let cancel = control.clone();
        let trace = profiler.clone();
        let disconnect_task = tokio::spawn(async move {
            if disconnected.await.is_err() {
                trace.cancellation_requested();
                cancel.cancel();
            }
        });
        started_rx.await.unwrap();
        drop(connected); // Client/request owner disappears while CPU work is running.
        disconnect_task.await.unwrap();
        worker.await.unwrap(); // Request return includes worker cleanup/join.
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

use howfar::ext::{ReportExt, WorkExt};
use howfar::{NoProgress, Report, Stop, StopReason, Unstoppable, Work};
use std::{num::NonZeroU64, sync::Mutex};

#[derive(Default)]
struct Log(Mutex<Vec<(&'static str, u64, u32)>>);
impl Report for Log {
    #[track_caller]
    fn advance(&self, n: u64) {
        self.0
            .lock()
            .unwrap()
            .push(("report", n, std::panic::Location::caller().line()));
    }
}
impl Stop for Log {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.0
            .lock()
            .unwrap()
            .push(("check", 0, std::panic::Location::caller().line()));
        Err(StopReason::Cancelled)
    }
}

#[test]
fn completed_work_survives_cancellation_and_preserves_the_call_site() {
    let log = Log::default();
    let work = Work::new(&log, Some(&log));
    let line = line!() + 1;
    assert_eq!(work.step(17), Err(StopReason::Cancelled));
    assert_eq!(
        *log.0.lock().unwrap(),
        [("report", 17, line), ("check", 0, line)]
    );
}

#[test]
fn absent_sinks_and_policies_are_permanent_no_ops() {
    let work = Work::new(Unstoppable, NoProgress);
    assert!(!work.may_stop());
    assert!(!work.may_report());
    assert_eq!(std::mem::size_of_val(&work), 0);
    work.step(u64::MAX).unwrap();
    let report: Option<&dyn Report> = None;
    assert!(!report.may_report());
    report.advance(1);
}

#[test]
fn worker_batch_flushes_partial_tail_and_does_not_wrap() {
    let log = Log::default();
    {
        let mut batch = (&log).batched(NonZeroU64::new(16).unwrap());
        batch.advance(15);
        assert!(log.0.lock().unwrap().is_empty());
        batch.advance(1);
        batch.advance(1);
        assert_eq!(batch.pending(), 1);
    }
    let values: Vec<_> = log.0.lock().unwrap().iter().map(|x| x.1).collect();
    assert_eq!(values, [16, 1]);
    let mut batch = (&log).batched(NonZeroU64::new(u64::MAX).unwrap());
    batch.advance(u64::MAX - 1);
    batch.advance(2);
    batch.flush();
    let values: Vec<_> = log.0.lock().unwrap().iter().map(|x| x.1).collect();
    assert_eq!(values, [16, 1, u64::MAX - 1, 2]);
}

#[test]
fn batched_rows_report_actual_work_for_empty_and_partial_inputs() {
    for rows in [0, 1, 15, 16, 17, 33] {
        let log = Log::default();
        let data = vec![0; rows];
        let work = Work::new(Unstoppable, &log);
        work.check().unwrap();
        for chunk in data.chunks(16) {
            work.step(chunk.len() as u64).unwrap();
        }
        assert_eq!(
            log.0.lock().unwrap().iter().map(|x| x.1).sum::<u64>(),
            rows as u64
        );
    }
}

#[test]
fn references_and_erased_sinks_forward() {
    let mut log = Log::default();
    let report: &mut dyn Report = &mut log;
    Report::advance(&report, 3);
    assert_eq!(log.0.lock().unwrap()[0].1, 3);
}

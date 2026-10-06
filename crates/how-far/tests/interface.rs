//! Counting through every sink shape, without a tracker.

use how_far::{NoReport, ProgressExt, ProgressWithStop, Report, Stop, StopReason, Unstoppable};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

struct Count(AtomicU64);
impl Report for Count {
    fn advance(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }
}

/// A library that accepts any sink and reports actual batch lengths.
fn library_algorithm(rows: &[u8], progress: impl Report) {
    for chunk in rows.chunks(16) {
        progress.advance(chunk.len() as u64);
    }
}

#[test]
fn caller_owned_counter_counts_partial_batches() {
    for rows in [0, 1, 16, 17, 33] {
        let counter = Count(AtomicU64::new(0));
        library_algorithm(&vec![0; rows], &counter);
        assert_eq!(counter.0.load(Ordering::Relaxed), rows as u64);
    }
}

#[test]
fn no_report_is_zero_sized_and_permanent() {
    assert_eq!(std::mem::size_of::<NoReport>(), 0);
    assert!(!NoReport.may_report());
    library_algorithm(&[0; 17], NoReport);
    let silent = ProgressWithStop::new(Unstoppable, NoReport);
    assert_eq!(std::mem::size_of_val(&silent), 0);
    assert!(!silent.may_stop() && !silent.may_report());
    silent.step(u64::MAX).unwrap();
}

#[test]
fn owned_erased_borrowed_and_optional_sinks_share_one_interface() {
    let count = Arc::new(Count(AtomicU64::new(0)));
    library_algorithm(&[0; 17], count.clone());
    assert_eq!(count.0.load(Ordering::Relaxed), 17);
    let owned: Box<dyn Report> = Box::new(count.clone());
    let erased: &dyn Report = &owned;
    library_algorithm(&[0; 1], erased);
    let mut ignored: Box<dyn Report> = Box::new(NoReport);
    library_algorithm(&[0; 1], &mut ignored);
    assert!(!ignored.may_report());
    let absent: Option<Arc<dyn Report>> = None;
    assert!(!absent.may_report());
    library_algorithm(&[0; 17], absent);
    let present: Option<Arc<dyn Report>> = Some(count.clone());
    library_algorithm(&[0; 2], present);
    assert_eq!(count.0.load(Ordering::Relaxed), 20);
}

struct Site(Mutex<u32>);
impl Report for Site {
    #[track_caller]
    fn advance(&self, _: u64) {
        *self.0.lock().unwrap() = std::panic::Location::caller().line();
    }
}
impl Stop for Site {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        *self.0.lock().unwrap() = std::panic::Location::caller().line();
        Ok(())
    }
}

#[test]
fn forwarding_preserves_the_original_call_site() {
    let site = Arc::new(Site(Mutex::new(0)));
    let line = line!() + 1;
    Report::advance(&site, 1);
    assert_eq!(*site.0.lock().unwrap(), line);
    let erased: Option<Arc<dyn Report>> = Some(site.clone());
    let line = line!() + 1;
    erased.advance(1);
    assert_eq!(*site.0.lock().unwrap(), line);
    let pair = ProgressWithStop::new(site.clone(), NoReport);
    let line = line!() + 1;
    pair.step(1).unwrap();
    assert_eq!(*site.0.lock().unwrap(), line);
}

/// Records reports and checks in order, with the line each came from.
#[derive(Default)]
struct Log(Mutex<Vec<(&'static str, u64, u32)>>);
impl Report for Log {
    #[track_caller]
    fn advance(&self, n: u64) {
        let line = std::panic::Location::caller().line();
        self.0.lock().unwrap().push(("report", n, line));
    }
}
impl Stop for Log {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        let line = std::panic::Location::caller().line();
        self.0.lock().unwrap().push(("check", 0, line));
        Err(StopReason::Cancelled)
    }
}

#[test]
fn step_counts_finished_work_then_checks_at_the_callers_line() {
    let log = Log::default();
    let work = ProgressWithStop::new(&log, &log);
    let line = line!() + 1;
    assert_eq!(work.step(17), Err(StopReason::Cancelled));
    assert_eq!(
        *log.0.lock().unwrap(),
        [("report", 17, line), ("check", 0, line)]
    );
}

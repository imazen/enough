use howfar::{IgnoreProgress, Report};
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
fn library_algorithm(rows: &[u8], progress: impl Report) {
    for chunk in rows.chunks(16) {
        progress.advance(chunk.len() as u64);
    }
}
#[test]
fn caller_owned_counter_needs_no_tracker_and_counts_partial_batches() {
    for rows in [0, 1, 16, 17, 33] {
        let counter = Count(AtomicU64::new(0));
        library_algorithm(&vec![0; rows], &counter);
        assert_eq!(counter.0.load(Ordering::Relaxed), rows as u64);
    }
    assert_eq!(std::mem::size_of::<IgnoreProgress>(), 0);
    assert!(!IgnoreProgress.may_report());
    library_algorithm(&[0; 17], IgnoreProgress);
}
#[test]
fn owned_erased_and_borrowed_sinks_have_the_same_interface() {
    let count = Arc::new(Count(AtomicU64::new(0)));
    library_algorithm(&[0; 17], count.clone());
    assert_eq!(count.0.load(Ordering::Relaxed), 17);
    let owned: Box<dyn Report> = Box::new(count);
    let erased: &dyn Report = &owned;
    library_algorithm(&[0; 1], erased);
    let mut ignored: Box<dyn Report> = Box::new(IgnoreProgress);
    library_algorithm(&[0; 1], &mut ignored);
    assert!(!ignored.may_report());
    let absent: Option<Arc<dyn Report>> = None;
    assert!(!absent.may_report());
    library_algorithm(&[0; 17], absent);
}
struct Site(Mutex<u32>);
impl Report for Site {
    #[track_caller]
    fn advance(&self, _: u64) {
        *self.0.lock().unwrap() = std::panic::Location::caller().line();
    }
}
#[test]
fn forwarding_preserves_the_original_call_site() {
    let site = Arc::new(Site(Mutex::new(0)));
    let line = line!() + 1;
    Report::advance(&site, 1);
    assert_eq!(*site.0.lock().unwrap(), line);
}

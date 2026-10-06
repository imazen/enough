use how_far_along::Report;
use how_far_along::ext::ReportExt;
use std::{num::NonZeroU64, sync::Mutex};

#[derive(Default)]
struct Log(Mutex<Vec<u64>>);
impl Report for Log {
    fn advance(&self, n: u64) {
        self.0.lock().unwrap().push(n);
    }
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
    assert_eq!(*log.0.lock().unwrap(), [16, 1]);
    let mut batch = (&log).batched(NonZeroU64::new(u64::MAX).unwrap());
    batch.advance(u64::MAX - 1);
    batch.advance(2);
    batch.flush();
    assert_eq!(*log.0.lock().unwrap(), [16, 1, u64::MAX - 1, 2]);
}

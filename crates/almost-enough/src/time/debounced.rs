//! Debounced timeout that skips most `Instant::now()` calls.
//!
//! [`DebouncedTimeout`] wraps any [`Stop`] and adds deadline-based cancellation,
//! like [`WithTimeout`]. The key difference: it learns how fast `check()` is
//! being called and skips the expensive clock read on most calls.
//!
//! This matters for codecs and libraries where the caller controls the `Stop`
//! implementation but not the check frequency, and vice versa. A library
//! calling `stop.check()` every 4KB row can't know whether the caller passed a
//! `WithTimeout` (which calls `Instant::now()` every check) or a plain
//! `Stopper`. `DebouncedTimeout` lets callers add deadlines without imposing
//! a hidden ~17ns-per-check tax on the library's hot path.
//!
//! See [`DebouncedTimeout`]'s docs for how late it can stop.

use std::sync::atomic::{
    AtomicBool, AtomicU32, AtomicU64,
    Ordering::{Relaxed, SeqCst},
};
use std::time::{Duration, Instant};

use crate::{Stop, StopReason};

/// Default target interval between clock reads: 100μs (0.1ms).
///
/// This means we aim to call `Instant::now()` roughly every 100 microseconds,
/// regardless of how fast `check()` is called, but at least every
/// [`MAX_CHECKS_PER_CLOCK_READ`] checks.
const DEFAULT_TARGET_NANOS: u64 = 100_000;

/// The most checks between clock reads, however fast checks arrive.
///
/// A clock read costs about 100 instructions, so reading it every 64 checks
/// adds about 2 cycles per check, while bounding how late a timeout can come
/// after checks slow down: at most this many of the slower checks. Without
/// the bound, a timeout calibrated on back-to-back checks read the clock only
/// every 90,000 to 100,000 checks, and stopped up to 40 s late once checks
/// took 1 ms (`benchmarks/debounced-timeout-2026-10-06.md`).
const MAX_CHECKS_PER_CLOCK_READ: u32 = 64;

/// Convert a Duration to nanoseconds as u64, clamping at u64::MAX.
#[inline]
fn duration_to_nanos(d: Duration) -> u64 {
    d.as_nanos().min(u64::MAX as u128) as u64
}

/// A [`Stop`] wrapper that debounces the `Instant::now()` call.
///
/// After a brief calibration phase, `check()` only reads the clock every
/// N calls, where N is chosen so clock reads happen approximately once
/// per [`target_interval`](DebouncedTimeout::with_target_interval), and is
/// at most 64 (see [Deadline precision](#deadline-precision)).
///
/// **Adaptation behavior:**
/// - If calls slow down (longer between checks), increases check frequency
///   at the next clock read, to avoid missing the deadline.
/// - If calls speed up (shorter between checks), gradually decreases
///   check frequency to avoid over-checking.
///
/// # Deadline precision
///
/// It reads the clock about once per target interval (default 100μs), and
/// at least every 64 checks. While checks arrive at a steady rate it stops at
/// most about one target interval late, or one check late when checks come
/// further apart than that. It learns that checks have slowed
/// down only at its next clock read, so after a sudden slowdown (a library
/// moving on to a slower stage, say) it can stop up to 64 of the slower
/// checks late: 64 ms if they then come once a millisecond. [`WithTimeout`](super::WithTimeout)
/// stops at most one check late. If you need sub-100μs precision, either
/// lower the target with
/// [`with_target_interval`](DebouncedTimeout::with_target_interval) or use
/// [`WithTimeout`](super::WithTimeout) directly.
///
/// # When to Use
///
/// Prefer `DebouncedTimeout` over [`WithTimeout`](super::WithTimeout) when
/// the `Stop` implementation crosses an API boundary — the caller adds the
/// deadline, but the library controls check frequency. This avoids coupling
/// timeout overhead to the library's internal loop structure.
///
/// In tight loops (sub-microsecond between checks), the savings are ~10x.
/// In codec workloads (~4KB between checks), both types are equivalent
/// because the real work dwarfs the clock read.
///
/// # Example
///
/// ```rust
/// use almost_enough::{StopSource, Stop};
/// use almost_enough::time::DebouncedTimeout;
/// use std::time::Duration;
///
/// let source = StopSource::new();
/// let stop = DebouncedTimeout::new(source.as_ref(), Duration::from_millis(100));
///
/// // Fast loop — most check() calls skip the clock read
/// let mut i = 0u64;
/// while !stop.should_stop() {
///     i += 1;
///     if i > 1_000_000 { break; }
/// }
/// ```
pub struct DebouncedTimeout<T> {
    inner: T,
    /// Instant when this timeout was created (reference point for nanos math).
    created: Instant,
    /// Deadline as nanoseconds since `created`.
    deadline_nanos: u64,
    /// Target interval between clock reads, in nanoseconds.
    target_nanos: u64,

    /// When to read the clock next; reset by `clone` and the `tighten` methods.
    schedule: Schedule,
}

/// The clock-read schedule (atomics, so the timeout stays `Send + Sync`).
struct Schedule {
    /// Checks left before the next clock read. Only ever stored in
    /// `1..=MAX_CHECKS_PER_CLOCK_READ`; a check that takes it outside
    /// `2..=MAX_CHECKS_PER_CLOCK_READ` reads the clock, so racing checks that
    /// decrement past zero (wrapping) read it too.
    countdown: AtomicU32,
    /// Checks between clock reads: `1..=MAX_CHECKS_PER_CLOCK_READ`.
    skip_mod: AtomicU32,
    /// Nanoseconds since `created` at the last clock read.
    last_measured_nanos: AtomicU64,
    /// Set once a clock read finds the deadline passed.
    timed_out: AtomicBool,
}

impl Schedule {
    /// Read the clock on the first check, then calibrate.
    fn new() -> Self {
        Self {
            countdown: AtomicU32::new(1),
            skip_mod: AtomicU32::new(1),
            last_measured_nanos: AtomicU64::new(0),
            timed_out: AtomicBool::new(false),
        }
    }

    /// Count a check; true if this one should read the clock.
    #[inline(always)]
    fn due(&self) -> bool {
        !matches!(
            self.countdown.fetch_sub(1, Relaxed),
            2..=MAX_CHECKS_PER_CLOCK_READ
        )
    }
}

impl<T: Stop> DebouncedTimeout<T> {
    /// Create a new debounced timeout with the default target interval (100μs).
    ///
    /// The deadline is calculated as `Instant::now() + duration`.
    /// Durations longer than ~584 years are clamped to `u64::MAX` nanoseconds.
    #[inline]
    pub fn new(inner: T, duration: Duration) -> Self {
        let now = Instant::now();
        Self {
            inner,
            created: now,
            deadline_nanos: duration_to_nanos(duration),
            target_nanos: DEFAULT_TARGET_NANOS,
            schedule: Schedule::new(),
        }
    }

    /// Create a debounced timeout with an absolute deadline.
    ///
    /// If the deadline is in the past, the first clock read will trigger
    /// [`StopReason::TimedOut`].
    #[inline]
    pub fn with_deadline(inner: T, deadline: Instant) -> Self {
        let now = Instant::now();
        Self {
            inner,
            created: now,
            deadline_nanos: duration_to_nanos(deadline.saturating_duration_since(now)),
            target_nanos: DEFAULT_TARGET_NANOS,
            schedule: Schedule::new(),
        }
    }

    /// Set the target interval between clock reads.
    ///
    /// Smaller values check the clock more often (more responsive but more
    /// overhead). Larger values check less often (less overhead but may
    /// overshoot the deadline by up to this amount while checks arrive
    /// steadily). The clock is read at least every 64 checks regardless.
    ///
    /// Default: 100μs (0.1ms).
    #[inline]
    pub fn with_target_interval(mut self, interval: Duration) -> Self {
        self.target_nanos = duration_to_nanos(interval).max(1);
        self
    }

    /// Get the deadline as an `Instant`.
    #[inline]
    pub fn deadline(&self) -> Instant {
        self.created + Duration::from_nanos(self.deadline_nanos)
    }

    /// Get the remaining time until deadline.
    ///
    /// Returns `Duration::ZERO` if the deadline has passed.
    #[inline]
    pub fn remaining(&self) -> Duration {
        self.deadline().saturating_duration_since(Instant::now())
    }

    /// Get a reference to the inner stop.
    #[inline]
    pub fn inner(&self) -> &T {
        &self.inner
    }

    /// Unwrap and return the inner stop.
    #[inline]
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// Current number of `check()` calls between clock reads.
    ///
    /// Starts at 1 (every call) and adapts upward as the call rate is
    /// measured, up to 64.
    /// Useful for diagnostics and testing.
    #[inline]
    pub fn checks_per_clock_read(&self) -> u32 {
        self.schedule.skip_mod.load(Relaxed)
    }

    /// The cold path: read the clock, check the deadline, recalibrate.
    #[cold]
    #[inline(never)]
    fn measure_and_recalibrate(&self) -> bool {
        let schedule = &self.schedule;
        let elapsed_nanos = self.created.elapsed().as_nanos() as u64;
        if elapsed_nanos >= self.deadline_nanos {
            // Every later check reads the clock too, and stops.
            schedule.timed_out.store(true, SeqCst);
            schedule.countdown.store(1, SeqCst);
            return true; // timed out
        }

        // Recalibrate skip_mod from the rate since the last read, which was
        // `skip_mod` checks ago (exactly, unless threads share this timeout).
        let prev_nanos = schedule.last_measured_nanos.swap(elapsed_nanos, Relaxed);
        let current_skip = schedule.skip_mod.load(Relaxed);
        let delta_nanos = elapsed_nanos.saturating_sub(prev_nanos);
        let nanos_per_call = delta_nanos / u64::from(current_skip);
        // nanos_per_call is 0 when calls outpace the clock; keep the schedule then.
        let mut skip = current_skip;
        if let Some(ideal) = self.target_nanos.checked_div(nanos_per_call) {
            let ideal_skip = ideal.clamp(1, u64::from(MAX_CHECKS_PER_CLOCK_READ)) as u32;
            skip = if ideal_skip <= current_skip {
                // Calls are slower → need to check more often → adapt immediately
                ideal_skip
            } else {
                // Calls are faster → can check less often → adapt slowly (1/8 step)
                current_skip + (ideal_skip - current_skip).div_ceil(8)
            };
            schedule.skip_mod.store(skip, Relaxed);
        }
        // A thread that read the clock just before another timed out must not
        // undo that thread's `countdown = 1`: SeqCst makes it see the flag
        // whenever its store lands after that one.
        schedule.countdown.store(skip, SeqCst);
        if schedule.timed_out.load(SeqCst) {
            schedule.countdown.store(1, Relaxed);
        }
        false // not timed out
    }
}

impl<T: Stop> Stop for DebouncedTimeout<T> {
    #[inline]
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        // Always check the inner stop (typically a single atomic load).
        self.inner.check()?;

        // Hot path: count down to the next clock read.
        if !self.schedule.due() {
            return Ok(());
        }

        // Cold path: read clock, check deadline, recalibrate.
        if self.measure_and_recalibrate() {
            Err(StopReason::TimedOut)
        } else {
            Ok(())
        }
    }

    #[inline]
    #[track_caller]
    fn should_stop(&self) -> bool {
        if self.inner.should_stop() {
            return true;
        }

        if !self.schedule.due() {
            return false;
        }

        self.measure_and_recalibrate()
    }
}

impl<T: Stop> DebouncedTimeout<T> {
    /// Add another timeout, taking the tighter of the two deadlines.
    ///
    /// Resets calibration state since the new deadline may require
    /// a different check frequency.
    #[inline]
    pub fn tighten(self, duration: Duration) -> Self {
        let elapsed = duration_to_nanos(Instant::now().saturating_duration_since(self.created));
        let new_deadline_nanos = elapsed.saturating_add(duration_to_nanos(duration));
        let deadline_nanos = self.deadline_nanos.min(new_deadline_nanos);
        Self {
            inner: self.inner,
            created: self.created,
            deadline_nanos,
            target_nanos: self.target_nanos,
            schedule: Schedule::new(),
        }
    }

    /// Add another deadline, taking the earlier of the two.
    ///
    /// Resets calibration state since the new deadline may require
    /// a different check frequency.
    #[inline]
    pub fn tighten_deadline(self, deadline: Instant) -> Self {
        let new_deadline_nanos =
            duration_to_nanos(deadline.saturating_duration_since(self.created));
        let deadline_nanos = self.deadline_nanos.min(new_deadline_nanos);
        Self {
            inner: self.inner,
            created: self.created,
            deadline_nanos,
            target_nanos: self.target_nanos,
            schedule: Schedule::new(),
        }
    }
}

impl<T: Clone + Stop> Clone for DebouncedTimeout<T> {
    /// Clone resets calibration state — each clone starts fresh.
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            created: self.created,
            deadline_nanos: self.deadline_nanos,
            target_nanos: self.target_nanos,
            schedule: Schedule::new(),
        }
    }
}

impl<T: core::fmt::Debug> core::fmt::Debug for DebouncedTimeout<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let deadline = self.created + Duration::from_nanos(self.deadline_nanos);
        f.debug_struct("DebouncedTimeout")
            .field("inner", &self.inner)
            .field("deadline", &deadline)
            .field("target_interval_us", &(self.target_nanos / 1_000))
            .field("skip_mod", &self.schedule.skip_mod.load(Relaxed))
            .finish()
    }
}

/// Extension trait for creating [`DebouncedTimeout`] wrappers.
///
/// Automatically implemented for all [`Stop`] types when the `std` feature
/// is enabled.
pub trait DebouncedTimeoutExt: Stop + Sized {
    /// Add a debounced timeout to this stop.
    ///
    /// Like [`TimeoutExt::with_timeout`](super::TimeoutExt::with_timeout),
    /// but skips most `Instant::now()` calls by learning the call rate.
    ///
    /// Default target interval between clock reads: 100μs.
    #[inline]
    fn with_debounced_timeout(self, duration: Duration) -> DebouncedTimeout<Self> {
        DebouncedTimeout::new(self, duration)
    }

    /// Add a debounced timeout with an absolute deadline.
    #[inline]
    fn with_debounced_deadline(self, deadline: Instant) -> DebouncedTimeout<Self> {
        DebouncedTimeout::with_deadline(self, deadline)
    }
}

impl<T: Stop> DebouncedTimeoutExt for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StopSource, Unstoppable};

    #[test]
    fn basic_timeout() {
        let source = StopSource::new();
        let stop = DebouncedTimeout::new(source.as_ref(), Duration::from_millis(50));

        assert!(!stop.should_stop());
        assert!(stop.check().is_ok());

        std::thread::sleep(Duration::from_millis(80));

        // After enough checks, should detect timeout
        for _ in 0..100 {
            if stop.should_stop() {
                return; // success
            }
        }
        panic!("should have detected timeout");
    }

    #[test]
    fn cancel_before_timeout() {
        let source = StopSource::new();
        let stop = DebouncedTimeout::new(source.as_ref(), Duration::from_secs(60));

        source.cancel();

        // Inner cancellation is always checked immediately
        assert!(stop.should_stop());
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn calibration_ramps_up() {
        let source = StopSource::new();
        let stop = DebouncedTimeout::new(source.as_ref(), Duration::from_secs(60));

        // Initial: check every call
        assert_eq!(stop.checks_per_clock_read(), 1);

        // Pump through calls so calibration kicks in
        for _ in 0..10_000 {
            let _ = stop.check();
        }

        // After enough calls, should be skipping some
        assert!(
            stop.checks_per_clock_read() > 1,
            "skip_mod should have increased, got {}",
            stop.checks_per_clock_read()
        );
    }

    #[test]
    fn remaining_accuracy() {
        let source = StopSource::new();
        let stop = DebouncedTimeout::new(source.as_ref(), Duration::from_secs(10));

        let remaining = stop.remaining();
        assert!(remaining > Duration::from_secs(9));
        assert!(remaining <= Duration::from_secs(10));
    }

    #[test]
    fn tighten_works() {
        let source = StopSource::new();
        let stop = DebouncedTimeout::new(source.as_ref(), Duration::from_secs(60))
            .tighten(Duration::from_secs(1));

        let remaining = stop.remaining();
        assert!(remaining < Duration::from_secs(2));
    }

    #[test]
    fn clone_resets_calibration() {
        let source = StopSource::new();
        let stop = DebouncedTimeout::new(source.as_ref(), Duration::from_secs(60));

        // Pump to get calibration going
        for _ in 0..10_000 {
            let _ = stop.check();
        }
        assert!(stop.checks_per_clock_read() > 1);

        // Clone resets
        let cloned = stop.clone();
        assert_eq!(cloned.checks_per_clock_read(), 1);
    }

    #[test]
    fn extension_trait() {
        use super::DebouncedTimeoutExt;
        let source = StopSource::new();
        let stop = source
            .as_ref()
            .with_debounced_timeout(Duration::from_secs(10));
        assert!(!stop.should_stop());
    }

    #[test]
    fn is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DebouncedTimeout<crate::StopRef<'_>>>();
    }

    #[test]
    fn zero_duration_immediate_timeout() {
        let source = StopSource::new();
        let stop = DebouncedTimeout::new(source.as_ref(), Duration::ZERO);

        // First call reads the clock (skip_mod starts at 1) and sees expiry
        assert_eq!(stop.check(), Err(StopReason::TimedOut));
    }

    #[test]
    fn deadline_in_the_past() {
        let source = StopSource::new();
        let past = Instant::now() - Duration::from_secs(1);
        let stop = DebouncedTimeout::with_deadline(source.as_ref(), past);

        // deadline_nanos is 0 (saturating_duration_since clamps to zero)
        assert_eq!(stop.check(), Err(StopReason::TimedOut));
    }

    #[test]
    fn with_deadline_basic() {
        let source = StopSource::new();
        let deadline = Instant::now() + Duration::from_millis(100);
        let stop = DebouncedTimeout::with_deadline(source.as_ref(), deadline);

        assert!(!stop.should_stop());

        std::thread::sleep(Duration::from_millis(150));

        // Pump enough calls to trigger a clock read
        for _ in 0..100 {
            if stop.should_stop() {
                return;
            }
        }
        panic!("should have detected timeout");
    }

    #[test]
    fn deadline_accessor() {
        let source = StopSource::new();
        let stop = DebouncedTimeout::new(source.as_ref(), Duration::from_secs(10));

        let deadline = stop.deadline();
        let remaining = stop.remaining();
        assert!(remaining > Duration::from_secs(9));
        assert!(remaining <= Duration::from_secs(10));
        // deadline should be ~10s from now
        assert!(deadline > Instant::now() + Duration::from_secs(9));
    }

    #[test]
    fn inner_access() {
        let source = StopSource::new();
        let stop = DebouncedTimeout::new(source.as_ref(), Duration::from_secs(10));

        assert!(!stop.inner().should_stop());

        source.cancel();

        assert!(stop.inner().should_stop());
    }

    #[test]
    fn into_inner_works() {
        let source = StopSource::new();
        let stop = DebouncedTimeout::new(source.as_ref(), Duration::from_secs(10));

        let inner = stop.into_inner();
        assert!(!inner.should_stop());
    }

    #[test]
    fn tighten_deadline_works() {
        let source = StopSource::new();
        let stop = DebouncedTimeout::new(source.as_ref(), Duration::from_secs(60))
            .tighten_deadline(Instant::now() + Duration::from_secs(1));

        let remaining = stop.remaining();
        assert!(remaining < Duration::from_secs(2));
    }

    #[test]
    fn tighten_does_not_loosen() {
        let source = StopSource::new();
        let stop = DebouncedTimeout::new(source.as_ref(), Duration::from_secs(1))
            .tighten(Duration::from_secs(60));

        // Should still be ~1 second, not 60
        let remaining = stop.remaining();
        assert!(remaining < Duration::from_secs(2));
    }

    #[test]
    fn debug_format() {
        let source = StopSource::new();
        let stop = DebouncedTimeout::new(source.as_ref(), Duration::from_secs(10));
        let debug = format!("{stop:?}");
        assert!(debug.contains("DebouncedTimeout"));
        assert!(debug.contains("skip_mod"));
        assert!(debug.contains("target_interval_us"));
    }

    #[test]
    fn with_target_interval_zero_clamps_to_one() {
        let source = StopSource::new();
        let stop = DebouncedTimeout::new(source.as_ref(), Duration::from_secs(60))
            .with_target_interval(Duration::ZERO);

        // Should still work (target_nanos clamped to 1, not 0)
        assert!(stop.check().is_ok());
    }

    #[test]
    fn with_debounced_deadline_ext() {
        let source = StopSource::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        let stop = source.as_ref().with_debounced_deadline(deadline);

        assert!(!stop.should_stop());
        assert!(stop.remaining() > Duration::from_secs(9));
    }

    #[test]
    fn check_and_should_stop_agree() {
        let source = StopSource::new();
        let stop = DebouncedTimeout::new(source.as_ref(), Duration::from_secs(60));

        // Both should report not stopped
        for _ in 0..1000 {
            assert!(!stop.should_stop());
            assert!(stop.check().is_ok());
        }

        source.cancel();

        // Both should report stopped (inner cancellation is immediate)
        assert!(stop.should_stop());
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn remaining_after_expiry_is_zero() {
        let source = StopSource::new();
        let stop = DebouncedTimeout::new(source.as_ref(), Duration::from_millis(1));

        std::thread::sleep(Duration::from_millis(10));

        assert_eq!(stop.remaining(), Duration::ZERO);
    }

    #[test]
    fn adapts_to_slowdown() {
        let source = StopSource::new();
        // Use a smaller target interval so skip_mod stays manageable for testing.
        let stop = DebouncedTimeout::new(source.as_ref(), Duration::from_secs(60))
            .with_target_interval(Duration::from_micros(10));

        // Fast phase: pump quickly to ramp up skip_mod
        for _ in 0..50_000 {
            let _ = stop.check();
        }
        let fast_skip = stop.checks_per_clock_read();
        assert!(fast_skip > 1, "should have ramped up, got {fast_skip}");

        // Slow phase: sleep between calls. Need enough calls to trigger
        // at least one recalibration (count % skip_mod == 0).
        for _ in 0..(fast_skip as usize + 100) {
            std::thread::sleep(Duration::from_micros(50));
            let _ = stop.check();
        }

        let slow_skip = stop.checks_per_clock_read();
        assert!(
            slow_skip < fast_skip,
            "should have reduced skip_mod from {fast_skip} to less, got {slow_skip}"
        );
    }

    /// Spin for `duration`, as a check-to-check gap of real work would.
    fn work_for(duration: Duration) {
        let start = Instant::now();
        while start.elapsed() < duration {
            std::hint::spin_loop();
        }
    }

    #[test]
    fn reads_the_clock_at_least_every_64_checks() {
        let stop = DebouncedTimeout::new(Unstoppable, Duration::from_secs(60));
        for _ in 0..1_000_000 {
            stop.check().unwrap();
            assert!(stop.checks_per_clock_read() <= MAX_CHECKS_PER_CLOCK_READ);
        }
    }

    /// Check back to back for 20 ms with a deadline far away, as a library's
    /// fast stage would, so the schedule calibrates to the cap.
    fn calibrated_on_fast_checks() -> DebouncedTimeout<Unstoppable> {
        let stop = DebouncedTimeout::new(Unstoppable, Duration::from_secs(3600));
        let fast_until = Instant::now() + Duration::from_millis(20);
        while Instant::now() < fast_until {
            stop.check().unwrap();
        }
        stop
    }

    #[test]
    fn a_passed_deadline_stops_within_64_checks() {
        let mut stop = calibrated_on_fast_checks();
        stop.deadline_nanos = 0;
        let mut ok_checks = 0;
        while stop.check().is_ok() {
            ok_checks += 1;
            assert!(ok_checks < MAX_CHECKS_PER_CLOCK_READ, "{ok_checks} checks");
        }
    }

    #[test]
    fn a_slowdown_after_calibration_stops_within_64_slow_checks() {
        // Calibrate on fast checks, then make every check take 1ms with the
        // deadline 20ms away. Without the bound, a release build calibrated
        // to one clock read per 90,000 to 100,000 checks and stopped seconds
        // late.
        let mut stop = calibrated_on_fast_checks();
        stop.deadline_nanos = duration_to_nanos(stop.created.elapsed()) + 20_000_000;
        let mut slow_checks = 0;
        while stop.check().is_ok() {
            slow_checks += 1;
            assert!(slow_checks < 1_000, "still running a second later");
            work_for(Duration::from_millis(1));
        }
        // At most 20 slow checks reach the deadline. The first clock read
        // after the slowdown comes within 64 checks, the next within a few
        // (it recalibrates on the slow ones), and then every check.
        assert!(slow_checks <= 20 + 64 + 8, "{slow_checks} slow checks");
    }

    #[test]
    fn a_clock_read_just_before_another_threads_timeout_keeps_it_stopped() {
        // Thread A timed out (the flag, then `countdown = 1`); thread B read
        // the clock just before the deadline and recalibrates afterwards.
        let stop = calibrated_on_fast_checks();
        stop.schedule.timed_out.store(true, SeqCst);
        stop.schedule.countdown.store(1, SeqCst);
        assert!(!stop.measure_and_recalibrate());
        assert_eq!(stop.schedule.countdown.load(Relaxed), 1);
    }

    #[test]
    fn once_timed_out_every_check_stops() {
        let stop = DebouncedTimeout::new(Unstoppable, Duration::ZERO);
        for _ in 0..10_000 {
            assert_eq!(stop.check(), Err(StopReason::TimedOut));
            assert!(stop.should_stop());
        }
    }

    #[test]
    fn threads_sharing_a_timeout_all_stop() {
        // Racing checks decrement the countdown past zero; every thread must
        // still reach a clock read and stop.
        let stop = DebouncedTimeout::new(Unstoppable, Duration::from_millis(20));
        let start = Instant::now();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    while stop.check().is_ok() {
                        assert!(start.elapsed() < Duration::from_secs(10));
                    }
                });
            }
        });
        assert_eq!(stop.check(), Err(StopReason::TimedOut));
    }
}

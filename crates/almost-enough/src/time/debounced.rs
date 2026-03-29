//! Debounced timeout that skips most `Instant::now()` calls.
//!
//! [`DebouncedTimeout`] wraps any [`Stop`] and adds deadline-based cancellation,
//! like [`WithTimeout`]. The key difference: it learns how fast `check()` is
//! being called and skips the expensive clock read on most calls.
//!
//! This is useful when `check()` is called in a very tight loop where
//! `Instant::now()` (~15-25ns) dominates the per-iteration cost. In
//! codec-style workloads with real computation between checks, use
//! [`WithTimeout`] instead — the clock read is invisible next to the work.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use crate::{Stop, StopReason};

/// Default target interval between clock reads: 100μs (0.1ms).
///
/// This means we aim to call `Instant::now()` roughly every 100 microseconds,
/// regardless of how fast `check()` is called. At 10ns per call, that's one
/// clock read per ~10,000 checks.
const DEFAULT_TARGET_NANOS: u64 = 100_000;

/// A [`Stop`] wrapper that debounces the `Instant::now()` call.
///
/// After a brief calibration phase, `check()` only reads the clock every
/// N calls, where N is chosen so clock reads happen approximately once
/// per [`target_interval`](DebouncedTimeout::with_target_interval).
///
/// **Adaptation behavior:**
/// - If calls slow down (longer between checks), immediately increases
///   check frequency to avoid missing the deadline.
/// - If calls speed up (shorter between checks), gradually decreases
///   check frequency to avoid over-checking.
///
/// # When to Use
///
/// Use `DebouncedTimeout` when `check()` is called in a tight loop with
/// very little work between calls (sub-microsecond). For codec-style workloads
/// with real computation between checks, [`WithTimeout`](super::WithTimeout)
/// is simpler and equally fast.
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

    // ── Mutable state (atomics for Send+Sync) ──────────────────────
    /// Monotonic call counter (wraps at u32::MAX, which is fine).
    call_count: AtomicU32,
    /// Check the clock when `call_count % skip_mod == 0`. Minimum 1.
    skip_mod: AtomicU32,
    /// Nanoseconds since `created` at the last clock read.
    last_measured_nanos: AtomicU64,
    /// `call_count` value at the last clock read.
    last_measured_count: AtomicU32,
}

impl<T: Stop> DebouncedTimeout<T> {
    /// Create a new debounced timeout with the default target interval (100μs).
    ///
    /// The deadline is calculated as `Instant::now() + duration`.
    #[inline]
    pub fn new(inner: T, duration: Duration) -> Self {
        let now = Instant::now();
        Self {
            inner,
            created: now,
            deadline_nanos: duration.as_nanos() as u64,
            target_nanos: DEFAULT_TARGET_NANOS,
            call_count: AtomicU32::new(0),
            skip_mod: AtomicU32::new(1),
            last_measured_nanos: AtomicU64::new(0),
            last_measured_count: AtomicU32::new(0),
        }
    }

    /// Create a debounced timeout with an absolute deadline.
    #[inline]
    pub fn with_deadline(inner: T, deadline: Instant) -> Self {
        let now = Instant::now();
        Self {
            inner,
            created: now,
            deadline_nanos: deadline.saturating_duration_since(now).as_nanos() as u64,
            target_nanos: DEFAULT_TARGET_NANOS,
            call_count: AtomicU32::new(0),
            skip_mod: AtomicU32::new(1),
            last_measured_nanos: AtomicU64::new(0),
            last_measured_count: AtomicU32::new(0),
        }
    }

    /// Set the target interval between clock reads.
    ///
    /// Smaller values check the clock more often (more responsive but more
    /// overhead). Larger values check less often (less overhead but may
    /// overshoot the deadline by up to this amount).
    ///
    /// Default: 100μs (0.1ms).
    #[inline]
    pub fn with_target_interval(mut self, interval: Duration) -> Self {
        self.target_nanos = interval.as_nanos().max(1) as u64;
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
    /// Starts at 1 (every call) and adapts upward as the call rate is measured.
    /// Useful for diagnostics and testing.
    #[inline]
    pub fn checks_per_clock_read(&self) -> u32 {
        self.skip_mod.load(Relaxed)
    }

    /// The cold path: read the clock, check the deadline, recalibrate.
    #[cold]
    #[inline(never)]
    fn measure_and_recalibrate(&self, count: u32) -> bool {
        let elapsed_nanos = self.created.elapsed().as_nanos() as u64;

        if elapsed_nanos >= self.deadline_nanos {
            return true; // timed out
        }

        // Recalibrate skip_mod based on observed call rate.
        let prev_nanos = self.last_measured_nanos.swap(elapsed_nanos, Relaxed);
        let prev_count = self.last_measured_count.swap(count, Relaxed);

        let delta_nanos = elapsed_nanos.saturating_sub(prev_nanos);
        let delta_calls = count.wrapping_sub(prev_count) as u64;

        if delta_calls > 0 && delta_nanos > 0 {
            let nanos_per_call = delta_nanos / delta_calls;
            if nanos_per_call > 0 {
                let ideal_skip =
                    (self.target_nanos / nanos_per_call).clamp(1, u32::MAX as u64) as u32;
                let current_skip = self.skip_mod.load(Relaxed);

                let new_skip = if ideal_skip <= current_skip {
                    // Calls are slower → need to check more often → adapt immediately
                    ideal_skip
                } else {
                    // Calls are faster → can check less often → adapt slowly (1/8 step)
                    current_skip
                        .saturating_add((ideal_skip - current_skip) / 8)
                        .max(1)
                };

                self.skip_mod.store(new_skip, Relaxed);
            }
        }

        false // not timed out
    }
}

impl<T: Stop> Stop for DebouncedTimeout<T> {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        // Always check the inner stop (typically a single atomic load).
        self.inner.check()?;

        // Increment call counter and decide whether to read the clock.
        let count = self.call_count.fetch_add(1, Relaxed).wrapping_add(1);
        let skip = self.skip_mod.load(Relaxed);

        // Hot path: skip the clock read.
        if !count.is_multiple_of(skip) {
            return Ok(());
        }

        // Cold path: read clock, check deadline, recalibrate.
        if self.measure_and_recalibrate(count) {
            Err(StopReason::TimedOut)
        } else {
            Ok(())
        }
    }

    #[inline]
    fn should_stop(&self) -> bool {
        if self.inner.should_stop() {
            return true;
        }

        let count = self.call_count.fetch_add(1, Relaxed).wrapping_add(1);
        let skip = self.skip_mod.load(Relaxed);

        if !count.is_multiple_of(skip) {
            return false;
        }

        self.measure_and_recalibrate(count)
    }
}

impl<T: Stop> DebouncedTimeout<T> {
    /// Add another timeout, taking the tighter of the two deadlines.
    ///
    /// Resets calibration state since the new deadline may require
    /// a different check frequency.
    #[inline]
    pub fn tighten(self, duration: Duration) -> Self {
        let new_deadline_nanos = Instant::now()
            .saturating_duration_since(self.created)
            .as_nanos() as u64
            + duration.as_nanos() as u64;
        let deadline_nanos = self.deadline_nanos.min(new_deadline_nanos);
        Self {
            inner: self.inner,
            created: self.created,
            deadline_nanos,
            target_nanos: self.target_nanos,
            call_count: AtomicU32::new(0),
            skip_mod: AtomicU32::new(1),
            last_measured_nanos: AtomicU64::new(0),
            last_measured_count: AtomicU32::new(0),
        }
    }

    /// Add another deadline, taking the earlier of the two.
    ///
    /// Resets calibration state since the new deadline may require
    /// a different check frequency.
    #[inline]
    pub fn tighten_deadline(self, deadline: Instant) -> Self {
        let new_deadline_nanos = deadline.saturating_duration_since(self.created).as_nanos() as u64;
        let deadline_nanos = self.deadline_nanos.min(new_deadline_nanos);
        Self {
            inner: self.inner,
            created: self.created,
            deadline_nanos,
            target_nanos: self.target_nanos,
            call_count: AtomicU32::new(0),
            skip_mod: AtomicU32::new(1),
            last_measured_nanos: AtomicU64::new(0),
            last_measured_count: AtomicU32::new(0),
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
            call_count: AtomicU32::new(0),
            skip_mod: AtomicU32::new(1),
            last_measured_nanos: AtomicU64::new(0),
            last_measured_count: AtomicU32::new(0),
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
            .field("skip_mod", &self.skip_mod.load(Relaxed))
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
    use crate::StopSource;

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
}

//! # enough-tokio
//!
//! Bridge tokio's `CancellationToken` to the [`Stop`] trait.
//!
//! ## When to Use
//!
//! Use this crate when you have:
//! - Tokio async code that needs to cancel CPU-intensive sync work in `spawn_blocking`
//! - Libraries that accept `impl Stop` and you want to use tokio's cancellation
//!
//! ## Complete Example
//!
//! ```rust,no_run
//! use enough_tokio::TokioStop;
//! use enough::Stop;
//! use tokio_util::sync::CancellationToken;
//!
//! #[tokio::main]
//! async fn main() {
//!     let token = CancellationToken::new();
//!     let stop = TokioStop::new(token.clone());
//!
//!     // Spawn CPU-intensive work
//!     let handle = tokio::task::spawn_blocking(move || {
//!         for i in 0..1_000_000 {
//!             if i % 1000 == 0 && stop.should_stop() {
//!                 return Err("cancelled");
//!             }
//!             // ... do work ...
//!         }
//!         Ok("done")
//!     });
//!
//!     // Cancel after timeout
//!     tokio::time::sleep(std::time::Duration::from_millis(10)).await;
//!     token.cancel();
//!
//!     let result = handle.await.unwrap();
//!     println!("{:?}", result);
//! }
//! ```
//!
//! ## Quick Reference
//!
//! ```rust,no_run
//! # use enough_tokio::TokioStop;
//! # use enough::Stop;
//! # use tokio_util::sync::CancellationToken;
//! let token = CancellationToken::new();
//! let stop = TokioStop::new(token.clone());
//!
//! stop.should_stop();         // Check if cancelled (sync)
//! stop.cancel();              // Trigger cancellation
//! // stop.cancelled().await;  // Wait for cancellation (async)
//! let child = stop.child();   // Create child token
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::all)]

use enough::{Stop, StopReason};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Wake, Waker};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

/// Checks a stop answers through the token before it registers a waker.
const REGISTER_AFTER: u32 = 32;

/// Wrapper around tokio's [`CancellationToken`] that implements [`Stop`].
///
/// This allows using tokio's cancellation system with libraries that
/// accept `impl Stop`.
///
/// # Cost
///
/// A new `TokioStop` costs what its token does: nothing is allocated or
/// registered. Its first 32 checks ask the token, whose
/// [`is_cancelled`](CancellationToken::is_cancelled) locks a mutex. A stop
/// checked more often than that registers a waker with the token, which
/// allocates twice and costs about as much as 20 checks, and from then on a
/// check reads the flag that waker sets. Measured on x86-64, a check costs
/// about 65 instructions while it asks the token and counts, and 15 once
/// registered (through `&dyn Stop`; `almost_enough::Stopper`: 10).
/// Cancelling the token sets the flag before `cancel` returns.
///
/// So stops that live briefly, such as one per request on a shared shutdown
/// token, cost what the token costs, and `cancel` wakes only the stops that
/// are checked in loops. Each clone counts and registers on its own.
///
/// # Example
///
/// ```rust
/// use enough_tokio::TokioStop;
/// use enough::Stop;
/// use tokio_util::sync::CancellationToken;
///
/// let token = CancellationToken::new();
/// let stop = TokioStop::new(token.clone());
///
/// assert!(!stop.should_stop());
///
/// token.cancel();
///
/// assert!(stop.should_stop());
/// ```
pub struct TokioStop {
    token: CancellationToken,
    /// Checks answered through the token, counted until the stop registers.
    /// Racing checks may lose an increment, which only delays registration.
    checks: AtomicU32,
    /// The waker's flag, once registered.
    fast: OnceLock<Registration>,
}

/// A waker registered with the token, and the flag it sets.
struct Registration {
    /// Set by the waker. A check reads only this, then asks the token, so a
    /// spurious wake costs speed, never a false stop.
    woken: Arc<Woken>,
    /// Registered with the token by its first poll and never polled again:
    /// it lives only so that cancelling the token wakes `woken`. Nothing
    /// observes it, so a panic cannot leave it half-updated.
    _waiter: AssertUnwindSafe<Pin<Box<WaitForCancellationFutureOwned>>>,
}

impl Registration {
    fn new(token: &CancellationToken) -> Self {
        let woken = Arc::new(Woken(AtomicBool::new(false)));
        let mut waiter = Box::pin(token.clone().cancelled_owned());
        let waker = Waker::from(Arc::clone(&woken));
        // Polling once registers the waiter, so cancelling the token wakes
        // it. The poll is `Ready` if the token is already cancelled. It can
        // also be `Pending` without registering (tokio's task-dump tracing
        // does that), but only after the token's cancellation has begun, so
        // the token is asked again.
        let ready = waiter
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_ready();
        if ready || token.is_cancelled() {
            woken.wake_by_ref();
        }
        Self {
            woken,
            _waiter: AssertUnwindSafe(waiter),
        }
    }
}

/// The waker the waiter holds. It owns only the flag, not the waiter, so the
/// waiter cannot keep itself alive through it.
struct Woken(AtomicBool);

impl Wake for Woken {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }
}

impl TokioStop {
    /// Create a new TokioStop from a CancellationToken.
    #[inline]
    pub fn new(token: CancellationToken) -> Self {
        Self {
            token,
            checks: AtomicU32::new(0),
            fast: OnceLock::new(),
        }
    }

    /// Get the underlying CancellationToken.
    #[inline]
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// Get a clone of the underlying CancellationToken.
    #[inline]
    pub fn into_token(self) -> CancellationToken {
        self.token
    }

    /// Wait for cancellation.
    ///
    /// This is an async method for use in async contexts.
    #[inline]
    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }

    /// Create a child token that is cancelled when this one is.
    #[inline]
    pub fn child(&self) -> TokioStop {
        Self::new(self.token.child_token())
    }

    /// Cancel the token.
    #[inline]
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// Ask the token, counting toward registration.
    #[inline(never)]
    fn ask_token(&self) -> bool {
        if self.token.is_cancelled() {
            return true;
        }
        let checks = self.checks.load(Ordering::Relaxed).saturating_add(1);
        self.checks.store(checks, Ordering::Relaxed);
        if checks >= REGISTER_AFTER {
            self.fast.get_or_init(|| Registration::new(&self.token));
        }
        false
    }

    /// Whether the token is cancelled, once the waker has fired.
    #[cold]
    #[inline(never)]
    fn confirm(&self) -> bool {
        self.token.is_cancelled()
    }
}

impl Clone for TokioStop {
    /// A clone shares the token; it counts and registers on its own.
    fn clone(&self) -> Self {
        Self::new(self.token.clone())
    }
}

impl Stop for TokioStop {
    #[inline]
    fn check(&self) -> Result<(), StopReason> {
        if self.should_stop() {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }

    #[inline]
    fn should_stop(&self) -> bool {
        match self.fast.get() {
            Some(fast) => fast.woken.0.load(Ordering::Acquire) && self.confirm(),
            None => self.ask_token(),
        }
    }
}

impl From<CancellationToken> for TokioStop {
    fn from(token: CancellationToken) -> Self {
        Self::new(token)
    }
}

impl From<TokioStop> for CancellationToken {
    fn from(stop: TokioStop) -> Self {
        stop.into_token()
    }
}

impl std::fmt::Debug for TokioStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokioStop")
            .field("cancelled", &self.token.is_cancelled())
            .finish()
    }
}

/// Extension trait for CancellationToken to easily convert to Stop.
///
/// Named `CancellationTokenStopExt` to avoid potential conflicts if
/// `tokio_util` ever adds a `CancellationTokenExt` trait.
pub trait CancellationTokenStopExt {
    /// Convert to a TokioStop for use with `impl Stop` APIs.
    fn as_stop(&self) -> TokioStop;
}

impl CancellationTokenStopExt for CancellationToken {
    fn as_stop(&self) -> TokioStop {
        TokioStop::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokio_stop_reflects_token() {
        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());

        assert!(!stop.should_stop());
        assert!(stop.check().is_ok());

        token.cancel();

        assert!(stop.should_stop());
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn tokio_stop_child() {
        let parent = TokioStop::new(CancellationToken::new());
        let child = parent.child();

        assert!(!child.should_stop());

        parent.cancel();

        assert!(child.should_stop());
    }

    #[test]
    fn tokio_stop_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TokioStop>();
    }

    #[test]
    fn tokio_stop_may_stop() {
        let token = CancellationToken::new();
        let stop = TokioStop::new(token);
        assert!(stop.may_stop());
    }

    #[test]
    fn tokio_stop_clone() {
        let token = CancellationToken::new();
        let stop1 = TokioStop::new(token.clone());
        let stop2 = stop1.clone();

        token.cancel();

        assert!(stop1.should_stop());
        assert!(stop2.should_stop());
    }

    #[test]
    fn from_conversions() {
        let token = CancellationToken::new();
        let stop: TokioStop = token.clone().into();
        let _token2: CancellationToken = stop.into();
    }

    #[test]
    fn extension_trait() {
        let token = CancellationToken::new();
        let stop = token.as_stop();

        assert!(!stop.should_stop());
        token.cancel();
        assert!(stop.should_stop());
    }

    #[tokio::test]
    async fn cancelled_async() {
        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());

        // Spawn a task that cancels after a delay
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            token.cancel();
        });

        // Wait for cancellation
        stop.cancelled().await;

        assert!(stop.should_stop());
    }

    #[tokio::test]
    async fn spawn_blocking_integration() {
        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());

        let handle = tokio::task::spawn_blocking(move || {
            let mut count = 0;
            for i in 0..1_000_000 {
                if i % 1000 == 0 && stop.should_stop() {
                    return Err("cancelled");
                }
                count += 1;
                // Simulate work
                std::hint::black_box(count);
            }
            Ok(count)
        });

        // Cancel quickly
        tokio::time::sleep(std::time::Duration::from_micros(100)).await;
        token.cancel();

        let result = handle.await.unwrap();
        // Either completed or cancelled - both are valid
        assert!(result.is_ok() || result == Err("cancelled"));
    }

    #[tokio::test]
    async fn select_with_cancellation() {
        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());

        // Spawn cancellation
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            token.cancel();
        });

        let result = tokio::select! {
            _ = stop.cancelled() => "cancelled",
            _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => "timeout",
        };

        assert_eq!(result, "cancelled");
    }

    #[tokio::test]
    async fn multiple_tasks_same_token() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let token = CancellationToken::new();
        let cancelled_count = Arc::new(AtomicUsize::new(0));

        let mut handles = vec![];

        for _ in 0..10 {
            let stop = TokioStop::new(token.clone());
            let cancelled_count = Arc::clone(&cancelled_count);

            handles.push(tokio::spawn(async move {
                for _ in 0..100 {
                    if stop.should_stop() {
                        cancelled_count.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            }));
        }

        // Cancel after some tasks have started
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        token.cancel();

        for h in handles {
            h.await.unwrap();
        }

        // At least some tasks should have been cancelled
        assert!(cancelled_count.load(Ordering::Relaxed) > 0);
    }

    #[tokio::test]
    async fn child_token_cancellation() {
        let parent = TokioStop::new(CancellationToken::new());
        let child1 = parent.child();
        let child2 = parent.child();

        assert!(!child1.should_stop());
        assert!(!child2.should_stop());

        // Cancel one child doesn't affect others
        child1.cancel();
        assert!(child1.should_stop());
        assert!(!child2.should_stop());
        assert!(!parent.should_stop());

        // Cancel parent affects remaining children
        parent.cancel();
        assert!(child2.should_stop());
    }

    #[tokio::test]
    async fn nested_child_tokens() {
        let root = TokioStop::new(CancellationToken::new());
        let level1 = root.child();
        let level2 = level1.child();
        let level3 = level2.child();

        assert!(!level3.should_stop());

        root.cancel();

        assert!(level1.should_stop());
        assert!(level2.should_stop());
        assert!(level3.should_stop());
    }

    #[tokio::test]
    async fn check_returns_correct_reason() {
        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());

        assert_eq!(stop.check(), Ok(()));

        token.cancel();

        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[tokio::test]
    async fn debug_formatting() {
        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());

        let debug = format!("{:?}", stop);
        assert!(debug.contains("TokioStop"));
        assert!(debug.contains("cancelled"));
        assert!(debug.contains("false"));

        token.cancel();

        let debug = format!("{:?}", stop);
        assert!(debug.contains("true"));
    }

    #[tokio::test]
    async fn integration_with_stop_trait() {
        fn process_sync(data: &[u8], stop: impl Stop) -> Result<usize, &'static str> {
            for (i, _chunk) in data.chunks(100).enumerate() {
                if i % 10 == 0 && stop.should_stop() {
                    return Err("cancelled");
                }
            }
            Ok(data.len())
        }

        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());
        let data = vec![0u8; 10000];

        // Not cancelled - completes
        let result = process_sync(&data, stop.clone());
        assert_eq!(result, Ok(10000));

        // Cancel and retry
        token.cancel();
        let result = process_sync(&data, stop);
        assert_eq!(result, Err("cancelled"));
    }

    #[tokio::test]
    async fn token_accessor_methods() {
        let original_token = CancellationToken::new();
        let stop = TokioStop::new(original_token.clone());

        // token() returns reference
        let token_ref = stop.token();
        assert!(!token_ref.is_cancelled());

        // into_token() consumes and returns owned token
        let recovered_token = stop.into_token();
        assert!(!recovered_token.is_cancelled());

        // Original token still works
        original_token.cancel();
        assert!(recovered_token.is_cancelled());
    }

    #[test]
    fn sync_send_bounds() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}

        assert_send::<TokioStop>();
        assert_sync::<TokioStop>();
    }

    #[tokio::test]
    async fn rapid_cancel_check_cycle() {
        // Stress test rapid cancellation
        for _ in 0..100 {
            let token = CancellationToken::new();
            let stop = TokioStop::new(token.clone());

            assert!(!stop.should_stop());
            token.cancel();
            assert!(stop.should_stop());
        }
    }

    #[tokio::test]
    async fn select_loop_with_pinned_cancelled() {
        use tokio::sync::mpsc;

        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());
        let (tx, mut rx) = mpsc::channel::<i32>(10);

        // Send some messages
        tx.send(1).await.unwrap();
        tx.send(2).await.unwrap();
        tx.send(3).await.unwrap();

        // Spawn cancellation after messages
        let token_clone = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            token_clone.cancel();
        });

        // Correct pattern: pin the future outside the loop
        let cancelled = stop.cancelled();
        tokio::pin!(cancelled);

        let mut received = vec![];
        let mut was_cancelled = false;

        loop {
            tokio::select! {
                _ = &mut cancelled => {
                    was_cancelled = true;
                    break;
                }
                msg = rx.recv() => {
                    match msg {
                        Some(m) => received.push(m),
                        None => break,
                    }
                }
            }
        }

        assert_eq!(received, vec![1, 2, 3]);
        assert!(was_cancelled);
    }

    // ── Lazy registration and the cached flag ──────────────────────────────

    fn registered(stop: &TokioStop) -> bool {
        stop.fast.get().is_some()
    }

    fn woken(stop: &TokioStop) -> bool {
        stop.fast
            .get()
            .is_some_and(|fast| fast.woken.0.load(Ordering::Acquire))
    }

    /// Check `stop` until it stops, failing rather than hanging if it never does.
    fn spin_until_stopped(stop: &TokioStop) {
        let start = std::time::Instant::now();
        while stop.check().is_ok() {
            assert!(
                start.elapsed().as_secs() < 10,
                "the stop never saw the cancel"
            );
            std::hint::spin_loop();
        }
    }

    /// Check a stop until it registers its waker.
    fn hot(stop: TokioStop) -> TokioStop {
        for _ in 0..REGISTER_AFTER {
            assert_eq!(stop.check(), Ok(()));
        }
        assert!(registered(&stop));
        stop
    }

    #[test]
    fn a_stop_registers_only_after_its_32nd_check() {
        let stop = TokioStop::new(CancellationToken::new());
        assert!(!registered(&stop));
        for _ in 1..REGISTER_AFTER {
            stop.check().unwrap();
        }
        assert!(!registered(&stop));
        stop.check().unwrap();
        assert!(registered(&stop) && !woken(&stop));
    }

    #[test]
    fn a_cancelled_token_stops_before_and_after_registration() {
        let token = CancellationToken::new();
        let (cold, warm) = (
            TokioStop::new(token.clone()),
            hot(TokioStop::new(token.clone())),
        );
        token.cancel();
        assert_eq!(cold.check(), Err(StopReason::Cancelled));
        assert!(
            !registered(&cold),
            "a stopped check does not count toward registering"
        );
        assert!(woken(&warm));
        assert_eq!(warm.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn cancel_sets_the_flag_before_it_returns() {
        let token = CancellationToken::new();
        let stop = hot(TokioStop::new(token.clone()));
        token.cancel();
        assert!(woken(&stop));
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn registering_on_an_already_cancelled_token_sets_the_flag() {
        let token = CancellationToken::new();
        token.cancel();
        let registration = Registration::new(&token);
        assert!(registration.woken.0.load(Ordering::Acquire));
    }

    #[test]
    fn a_child_of_a_cancelled_token_starts_cancelled() {
        let parent = TokioStop::new(CancellationToken::new());
        parent.cancel();
        assert!(parent.child().should_stop());
        assert!(TokioStop::new(parent.token().child_token()).should_stop());
    }

    #[test]
    fn cancelling_an_ancestor_reaches_a_grandchild_whose_parent_was_dropped() {
        // Dropping the middle token's last handle moves its children to the
        // root; the grandchild's registration must survive the move.
        let root = CancellationToken::new();
        let middle = root.child_token();
        let leaf = hot(TokioStop::new(middle.child_token()));
        let sibling = hot(TokioStop::new(middle.child_token().child_token()));
        drop(middle);
        assert!(!leaf.should_stop() && !sibling.should_stop());
        root.cancel();
        assert!(woken(&leaf) && woken(&sibling));
        assert!(leaf.should_stop() && sibling.should_stop());
    }

    #[test]
    fn cancelling_through_any_handle_reaches_the_flag() {
        let stop = hot(TokioStop::new(CancellationToken::new()));
        stop.token().cancel();
        assert!(woken(&stop) && stop.should_stop());

        let stop = hot(TokioStop::new(CancellationToken::new()));
        let clone = stop.clone();
        clone.cancel();
        assert!(woken(&stop) && stop.should_stop());
    }

    #[test]
    fn a_spurious_wake_is_not_a_stop() {
        let token = CancellationToken::new();
        let stop = hot(TokioStop::new(token.clone()));
        Waker::from(Arc::clone(&stop.fast.get().unwrap().woken)).wake();
        assert!(woken(&stop));
        assert!(!stop.should_stop());
        assert_eq!(stop.check(), Ok(()));
        token.cancel();
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn clones_count_and_register_on_their_own() {
        let stop = hot(TokioStop::new(CancellationToken::new()));
        let clone = stop.clone();
        assert!(registered(&stop) && !registered(&clone));
        assert!(stop.token() == clone.token());
    }

    #[test]
    fn dropping_a_stop_releases_its_registration() {
        for cancel in [false, true] {
            let token = CancellationToken::new();
            let stop = hot(TokioStop::new(token.clone()));
            let flag = Arc::downgrade(&stop.fast.get().unwrap().woken);
            if cancel {
                token.cancel();
            }
            drop(stop);
            // The waiter holds the waker, which holds the flag; neither may
            // keep the other alive.
            assert!(flag.upgrade().is_none());
        }
    }

    #[test]
    fn hot_and_cold_stops_on_one_token_all_see_the_cancel() {
        let token = CancellationToken::new();
        let stops: Vec<_> = (0..100)
            .map(|i| {
                let stop = TokioStop::new(token.clone());
                if i % 2 == 0 { hot(stop) } else { stop }
            })
            .collect();
        // Drop every third one so the token's waiter list has holes.
        let stops: Vec<_> = stops.into_iter().step_by(3).collect();
        token.cancel();
        assert!(stops.iter().all(|stop| stop.should_stop()));
    }

    #[test]
    fn registration_racing_cancel_never_misses_it() {
        use std::sync::Barrier;
        for _ in 0..if cfg!(miri) { 20 } else { 2_000 } {
            let token = CancellationToken::new();
            let stop = TokioStop::new(token.child_token());
            for _ in 1..REGISTER_AFTER {
                stop.check().unwrap();
            }
            let start = Arc::new(Barrier::new(2));
            let canceller = std::thread::spawn({
                let (token, start) = (token.clone(), Arc::clone(&start));
                move || {
                    start.wait();
                    token.cancel();
                }
            });
            start.wait();
            // This check registers, racing the cancel.
            let _ = stop.check();
            canceller.join().unwrap();
            // `cancel` has returned, so the stop must see it.
            assert!(stop.should_stop());
        }
    }

    #[test]
    fn threads_sharing_one_stop_register_it_once_and_all_stop() {
        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| spin_until_stopped(&stop));
            }
            let start = std::time::Instant::now();
            while !registered(&stop) {
                assert!(start.elapsed().as_secs() < 10, "never registered");
                std::hint::spin_loop();
            }
            token.cancel();
        });
        assert!(woken(&stop));
    }

    #[test]
    fn checks_on_other_threads_see_the_cancel_and_what_preceded_it() {
        use std::sync::atomic::AtomicUsize;
        for _ in 0..if cfg!(miri) { 4 } else { 200 } {
            let token = CancellationToken::new();
            let stop = TokioStop::new(token.clone());
            let data = Arc::new(AtomicUsize::new(0));
            let checkers: Vec<_> = (0..4)
                .map(|_| {
                    let (stop, data) = (stop.clone(), Arc::clone(&data));
                    std::thread::spawn(move || {
                        spin_until_stopped(&stop);
                        // Err synchronizes with the cancel, so the write
                        // before it is visible.
                        data.load(Ordering::Relaxed)
                    })
                })
                .collect();
            data.store(42, Ordering::Relaxed);
            token.cancel();
            for checker in checkers {
                assert_eq!(checker.join().unwrap(), 42);
            }
        }
    }

    #[test]
    fn into_token_returns_the_same_token() {
        let token = CancellationToken::new();
        assert!(TokioStop::new(token.clone()).into_token() == token);
        let back: CancellationToken = TokioStop::new(token.clone()).into();
        assert!(back == token);
    }

    #[test]
    fn auto_traits_are_unchanged() {
        fn assert_traits<
            T: Send + Sync + Unpin + std::panic::UnwindSafe + std::panic::RefUnwindSafe,
        >() {
        }
        assert_traits::<TokioStop>();
    }

    #[test]
    fn works_inside_a_current_thread_runtime() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let token = CancellationToken::new();
            let stop = TokioStop::new(token.clone());
            let canceller = tokio::spawn(async move { token.cancel() });
            canceller.await.unwrap();
            assert!(stop.should_stop());
        });
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_blocking_loop_stops_when_cancelled() {
        let token = CancellationToken::new();
        // Built inside a runtime task, as `spawn_blocking` callers do.
        let stop = TokioStop::new(token.clone());
        let work = tokio::task::spawn_blocking(move || spin_until_stopped(&stop));
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        token.cancel();
        work.await.unwrap();
    }

    #[tokio::test]
    async fn select_biased_cancellation_priority() {
        use tokio::sync::mpsc;

        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());
        let (tx, mut rx) = mpsc::channel::<i32>(10);

        // Pre-cancel before loop
        token.cancel();

        // Send a message (channel should still have it)
        tx.send(42).await.unwrap();

        let cancelled = stop.cancelled();
        tokio::pin!(cancelled);

        // With biased, cancellation should win since it's first
        let result = tokio::select! {
            biased;
            _ = &mut cancelled => "cancelled",
            _ = rx.recv() => "received",
        };

        assert_eq!(result, "cancelled");
    }
}

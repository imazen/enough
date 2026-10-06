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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Wake, Waker};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

/// Wrapper around tokio's [`CancellationToken`] that implements [`Stop`].
///
/// This allows using tokio's cancellation system with libraries that
/// accept `impl Stop`.
///
/// # Cost
///
/// [`CancellationToken::is_cancelled`] locks a mutex. So that a check
/// doesn't, `new` registers a waker with the token, and a check reads the
/// flag that waker sets: one atomic load, as cheap as
/// `almost_enough::Stopper`, until the token is cancelled. Cancelling the
/// token sets the flag before `cancel` returns.
///
/// In exchange `new` (and [`child`](Self::child) and
/// [`as_stop`](CancellationTokenStopExt::as_stop)) makes three small
/// allocations and registers the waker: about 1,000 instructions on x86-64
/// with the drop, as much as 25 calls to `is_cancelled`. Create one
/// per operation rather than per check. Clones share the registration.
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
#[derive(Clone)]
pub struct TokioStop {
    /// Set by the waker registered with the token. A check reads only this
    /// until it is set, then asks the token, so a spurious wake costs speed,
    /// never a false stop.
    woken: Arc<Woken>,
    shared: Arc<Shared>,
}

struct Shared {
    token: CancellationToken,
    /// Registered with the token by its first poll and never polled again:
    /// it lives only so that cancelling the token wakes `woken`. Nothing
    /// observes it, so a panic cannot leave it half-updated.
    _waiter: AssertUnwindSafe<Pin<Box<WaitForCancellationFutureOwned>>>,
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
    ///
    /// Registers a waker with the token, which allocates; see
    /// [Cost](TokioStop#cost).
    pub fn new(token: CancellationToken) -> Self {
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
            shared: Arc::new(Shared {
                token,
                _waiter: AssertUnwindSafe(waiter),
            }),
        }
    }

    /// Get the underlying CancellationToken.
    #[inline]
    pub fn token(&self) -> &CancellationToken {
        &self.shared.token
    }

    /// Get a clone of the underlying CancellationToken.
    #[inline]
    pub fn into_token(self) -> CancellationToken {
        self.shared.token.clone()
    }

    /// Wait for cancellation.
    ///
    /// This is an async method for use in async contexts.
    #[inline]
    pub async fn cancelled(&self) {
        self.shared.token.cancelled().await;
    }

    /// Create a child token that is cancelled when this one is.
    #[inline]
    pub fn child(&self) -> TokioStop {
        Self::new(self.shared.token.child_token())
    }

    /// Cancel the token.
    #[inline]
    pub fn cancel(&self) {
        self.shared.token.cancel();
    }

    /// Whether the token is cancelled, once the waker has fired.
    #[cold]
    #[inline(never)]
    fn confirm(&self) -> bool {
        self.shared.token.is_cancelled()
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
        self.woken.0.load(Ordering::Acquire) && self.confirm()
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
            .field("cancelled", &self.shared.token.is_cancelled())
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

    // ── The cached flag ─────────────────────────────────────────────────────

    fn woken(stop: &TokioStop) -> bool {
        stop.woken.0.load(Ordering::Acquire)
    }

    #[test]
    fn cancel_sets_the_flag_before_it_returns() {
        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());
        assert!(!woken(&stop));
        token.cancel();
        assert!(woken(&stop));
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn an_already_cancelled_token_is_seen_at_construction() {
        let token = CancellationToken::new();
        token.cancel();
        let stop = TokioStop::new(token);
        assert!(woken(&stop));
        assert!(stop.should_stop());
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
        let leaf = TokioStop::new(middle.child_token());
        let sibling = TokioStop::new(middle.child_token().child_token());
        drop(middle);
        assert!(!leaf.should_stop() && !sibling.should_stop());
        root.cancel();
        assert!(leaf.should_stop() && sibling.should_stop());
    }

    #[test]
    fn cancelling_through_any_handle_reaches_the_flag() {
        let stop = TokioStop::new(CancellationToken::new());
        stop.token().cancel();
        assert!(woken(&stop) && stop.should_stop());

        let stop = TokioStop::new(CancellationToken::new());
        let clone = stop.clone();
        clone.cancel();
        assert!(stop.should_stop());
    }

    #[test]
    fn a_spurious_wake_is_not_a_stop() {
        let token = CancellationToken::new();
        let stop = TokioStop::new(token.clone());
        Waker::from(Arc::clone(&stop.woken)).wake();
        assert!(woken(&stop));
        assert!(!stop.should_stop());
        assert_eq!(stop.check(), Ok(()));
        token.cancel();
        assert_eq!(stop.check(), Err(StopReason::Cancelled));
    }

    #[test]
    fn clones_share_one_registration() {
        let stop = TokioStop::new(CancellationToken::new());
        let clone = stop.clone();
        assert!(Arc::ptr_eq(&stop.woken, &clone.woken));
        assert!(Arc::ptr_eq(&stop.shared, &clone.shared));
    }

    #[test]
    fn dropping_the_last_clone_releases_the_registration() {
        for cancel in [false, true] {
            let token = CancellationToken::new();
            let stop = TokioStop::new(token.clone());
            let clone = stop.clone();
            let (flag, shared) = (Arc::downgrade(&stop.woken), Arc::downgrade(&stop.shared));
            if cancel {
                token.cancel();
            }
            drop(stop);
            assert!(flag.upgrade().is_some(), "a clone is still alive");
            drop(clone);
            // The waiter holds the waker, which holds the flag; neither may
            // keep the other alive.
            assert!(shared.upgrade().is_none());
            assert!(flag.upgrade().is_none());
        }
    }

    #[test]
    fn many_stops_on_one_token_all_see_the_cancel() {
        let token = CancellationToken::new();
        let stops: Vec<_> = (0..100).map(|_| TokioStop::new(token.clone())).collect();
        // Drop every other one so the token's waiter list has holes.
        let stops: Vec<_> = stops.into_iter().step_by(2).collect();
        token.cancel();
        assert!(stops.iter().all(|stop| woken(stop) && stop.should_stop()));
    }

    #[test]
    fn construction_racing_cancel_never_misses_it() {
        use std::sync::Barrier;
        for _ in 0..if cfg!(miri) { 20 } else { 2_000 } {
            let token = CancellationToken::new();
            let start = Arc::new(Barrier::new(2));
            let canceller = std::thread::spawn({
                let (token, start) = (token.clone(), Arc::clone(&start));
                move || {
                    start.wait();
                    token.cancel();
                }
            });
            start.wait();
            let stop = TokioStop::new(token.child_token());
            canceller.join().unwrap();
            // `cancel` has returned, so the stop must see it.
            assert!(stop.should_stop());
        }
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
                        while stop.check().is_ok() {
                            std::hint::spin_loop();
                        }
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
        let work = tokio::task::spawn_blocking(move || {
            let mut spins = 0u64;
            while stop.check().is_ok() {
                spins += 1;
                std::hint::black_box(spins);
            }
            spins
        });
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        token.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(10), work)
            .await
            .expect("the loop saw the cancel")
            .unwrap();
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

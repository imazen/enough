//! Checkpoint helpers for every value that checks and counts.

use crate::{Child, Execution, PhaseSpec, PlanError, Pulse, Report, Stop, StopReason};
use alloc::vec::Vec;

/// Checkpoints for anything that both checks cancellation and counts work,
/// including `&dyn Pulse`, [`Child`], and
/// [`ProgressWithStop`](crate::ProgressWithStop).
pub trait ProgressExt: Stop + Report {
    /// Count `completed` finished units, then check for cancellation.
    ///
    /// Work finished before a stop request is still counted. `check()` alone
    /// stays the cheapest stop-only checkpoint; check once before a loop and
    /// `step` after each finished unit or batch.
    #[inline]
    #[track_caller]
    fn step(&self, completed: u64) -> Result<(), StopReason> {
        self.advance(completed);
        self.check()
    }

    /// [`Pulse::split`] into exactly `N` children, returned as an array.
    ///
    /// ```
    /// use how_far::{Execution, NoPulse, PhaseSpec, ProgressExt, Total};
    ///
    /// let [left, right] = NoPulse.split_array(Execution::ForkJoin, [
    ///     PhaseSpec::new("left", 1, Total::Exact(10)),
    ///     PhaseSpec::new("right", 1, Total::Exact(10)),
    /// ])?;
    /// # let _ = (left, right);
    /// # Ok::<(), how_far::PlanError>(())
    /// ```
    ///
    /// # Panics
    ///
    /// If the pulse breaks the [`Pulse::split`] contract by returning a
    /// different number of children than parts.
    fn split_array<const N: usize>(
        &self,
        execution: Execution,
        parts: [PhaseSpec<'_>; N],
    ) -> Result<[Child<'_>; N], PlanError>
    where
        Self: Pulse,
    {
        let children = self.split(execution, &parts)?;
        let found = children.len();
        Ok(children.try_into().unwrap_or_else(|_: Vec<Child<'_>>| {
            panic!("Pulse::split returned {found} children for {N} parts")
        }))
    }
}

impl<T: Stop + Report + ?Sized> ProgressExt for T {}

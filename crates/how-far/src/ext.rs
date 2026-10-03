//! Checkpoint helpers for every value that checks and counts.

use crate::{Child, Execution, NoPulse, PhaseSpec, PlanError, Pulse, Report, Stop, StopReason};

/// Checkpoints for anything that both checks cancellation and counts work,
/// including `&dyn Pulse`, [`Child`], and
/// [`ProgressWithStop`](crate::ProgressWithStop).
pub trait ProgressExt: Stop + Report {
    /// Count `completed` finished units, then check for cancellation.
    ///
    /// Work finished before a stop request is still counted. Check once
    /// before a loop and `step` after each finished unit or batch; `check()`
    /// alone is the stop-only checkpoint. With
    /// [`NoPulse`](struct@crate::NoPulse), `step` makes no call: the pulse is
    /// recognized by its address, a comparison a loop moves out of its body.
    #[inline]
    #[track_caller]
    fn step(&self, completed: u64) -> Result<(), StopReason> {
        if is_no_pulse(self) {
            return Ok(());
        }
        self.advance(completed);
        self.check()
    }

    /// The live pulse: this value, or `None` when it can neither stop nor
    /// report.
    ///
    /// [`step`](Self::step) already skips [`NoPulse`](struct@crate::NoPulse)
    /// without a call. `live` extends that to any pulse whose `may_stop` and
    /// `may_report` both return `false`, for the price of asking it once. Call
    /// it once before a hot loop. `Option<&P>` implements `Stop` and `Report`,
    /// so `check`, `advance` and `step` work on the result: they forward when
    /// the pulse may stop or report, and cost a branch, which a loop hoists,
    /// instead of a call when it can do neither. `may_stop` and `may_report`
    /// return `false` only for permanent no-ops, so gating early never drops a
    /// report or a stop request.
    ///
    /// ```
    /// use how_far::prelude::*;
    ///
    /// fn decode(rows: &[Vec<u8>], pulse: &dyn Pulse) -> Result<u64, how_far::StopReason> {
    ///     let pulse = pulse.live();
    ///     pulse.check()?;
    ///     let mut sum = 0;
    ///     for row in rows {
    ///         sum += row.iter().map(|&b| u64::from(b)).sum::<u64>();
    ///         pulse.step(1)?;
    ///     }
    ///     Ok(sum)
    /// }
    ///
    /// assert!(how_far::NoPulse.live().is_none());
    /// assert_eq!(decode(&[vec![1, 2], vec![3]], &how_far::NoPulse), Ok(6));
    /// ```
    #[inline]
    fn live(&self) -> Option<&Self> {
        if !is_no_pulse(self) && (self.may_stop() || self.may_report()) {
            Some(self)
        } else {
            None
        }
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
        match self.split(execution, &parts)?.try_into() {
            Ok(children) => Ok(children),
            Err(children) => wrong_child_count(children.len(), N),
        }
    }
}

impl<T: Stop + Report + ?Sized> ProgressExt for T {}

/// Whether `value` is the one [`NoPulse`](static@NoPulse), whatever reference
/// it arrived as. The size rules out a zero-sized value at the same address,
/// such as one ending another allocation. For a sized type the size is a
/// constant, so the test compiles away unless the type is one byte.
#[inline]
fn is_no_pulse<T: ?Sized>(value: &T) -> bool {
    core::ptr::addr_eq(value, &NoPulse)
        && core::mem::size_of_val(value) == core::mem::size_of::<NoPulse>()
}

/// Kept out of `split_array`, which is instantiated for every pulse type and
/// length it is called with.
#[cold]
#[inline(never)]
fn wrong_child_count(found: usize, parts: usize) -> ! {
    panic!("Pulse::split returned {found} children for {parts} parts")
}

//! Checkpoints that reach the pulse only every so many units.

use crate::{ProgressExt, Pulse, StopReason};
use core::fmt;

/// Counts completed work locally and reaches its pulse once every `every`
/// units: it reports the units counted so far, then checks for cancellation.
///
/// Reaching a live tree costs a few nanoseconds per checkpoint; a step that
/// does not reach it is a subtraction and a branch. Choose `every` so that the
/// work between checkpoints takes at least a microsecond, and no longer than
/// the cancellation latency you need. A pulse that can neither stop nor
/// report, such as [`NoPulse`](crate::NoPulse), is never reached, and in a
/// loop its steps compile to nothing.
///
/// Make one per worker, from the `&dyn Pulse` the work was given:
///
/// ```
/// use how_far::{Pulse, StopReason};
///
/// fn defilter(rows: &mut [Vec<u8>], pulse: &dyn Pulse) -> Result<(), StopReason> {
///     let mut pace = pulse.paced(64 * 1024);
///     for row in rows {
///         for i in 4..row.len() {
///             row[i] = row[i].wrapping_add(row[i - 4]);
///         }
///         pace.step(row.len() as u64)?;
///     }
///     Ok(())
/// }
///
/// defilter(&mut vec![vec![0; 64]; 3], &how_far::NoPulse)?;
/// # Ok::<(), StopReason>(())
/// ```
///
/// Units not yet reported are reported when the `Paced` is dropped, including
/// after an early return, so finished work is always counted. Flush before
/// splitting the pulse it reports to: a phase that split ignores reports.
pub struct Paced<'a> {
    pulse: Option<&'a dyn Pulse>,
    /// Units still to count before the next reach: `every` minus the pending
    /// units, so a step is one comparison and one subtraction.
    left: u64,
    every: u64,
}

impl fmt::Debug for Paced<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Paced")
            .field("live", &self.pulse.is_some())
            .field("pending", &(self.every - self.left))
            .field("every", &self.every)
            .finish()
    }
}

impl<'a> Paced<'a> {
    /// Reach `pulse` once every `every` units (at least 1).
    pub fn new(pulse: &'a dyn Pulse, every: u64) -> Self {
        let every = every.max(1);
        Self {
            pulse: pulse.live(),
            left: every,
            every,
        }
    }

    /// Count `completed` finished units. Once `every` units are pending,
    /// report them and check for cancellation.
    #[inline]
    #[track_caller]
    pub fn step(&mut self, completed: u64) -> Result<(), StopReason> {
        // Nothing below writes `pulse`, so in a loop this test is hoisted and
        // a pulse that cannot stop or report costs nothing per step.
        let Some(pulse) = self.pulse else {
            return Ok(());
        };
        if completed < self.left {
            self.left -= completed;
            Ok(())
        } else {
            let pending = (self.every - self.left).saturating_add(completed);
            self.left = self.every;
            reach(pulse, pending)
        }
    }

    /// Check for cancellation now, whatever is pending.
    #[inline]
    #[track_caller]
    pub fn check(&self) -> Result<(), StopReason> {
        match self.pulse {
            Some(pulse) => pulse.check(),
            None => Ok(()),
        }
    }

    /// Report every pending unit now.
    #[inline]
    #[track_caller]
    pub fn flush(&mut self) {
        if let Some(pulse) = self.pulse {
            if self.left != self.every {
                pulse.advance(self.every - self.left);
                self.left = self.every;
            }
        }
    }
}

/// The slow path of [`Paced::step`], kept out of the caller's loop. It takes
/// the units by value, so no `Paced` escapes into it and the caller can keep
/// one in registers.
#[cold]
#[inline(never)]
#[track_caller]
fn reach(pulse: &dyn Pulse, pending: u64) -> Result<(), StopReason> {
    pulse.advance(pending);
    pulse.check()
}

impl Drop for Paced<'_> {
    #[inline]
    fn drop(&mut self) {
        self.flush();
    }
}

impl<'a> dyn Pulse + 'a {
    /// Checkpoints that reach this pulse once every `every` units; see
    /// [`Paced`].
    pub fn paced(&self, every: u64) -> Paced<'_> {
        Paced::new(self, every)
    }
}

impl crate::Child<'_> {
    /// Checkpoints that reach this child's pulse once every `every` units; see
    /// [`Paced`].
    pub fn paced(&self, every: u64) -> Paced<'_> {
        Paced::new(self.pulse(), every)
    }
}

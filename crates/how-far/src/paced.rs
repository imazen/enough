//! Checkpoints that reach the pulse only every so many units.

use crate::{Pulse, StopReason};
use core::fmt;

/// Counts completed work locally and reaches its pulse once every `every`
/// units: it reports the units counted so far, then checks for cancellation.
///
/// Reaching a live tree costs a few nanoseconds per checkpoint; a step that
/// does not reach it costs an addition and a comparison. Choose `every` so
/// that the work between checkpoints takes at least a microsecond, and no
/// longer than the cancellation latency you need. A pulse that can neither
/// stop nor report, such as [`NoPulse`](crate::NoPulse), is never reached.
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
    pending: u64,
    every: u64,
}

impl fmt::Debug for Paced<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Paced")
            .field("live", &self.pulse.is_some())
            .field("pending", &self.pending)
            .field("every", &self.every)
            .finish()
    }
}

impl<'a> Paced<'a> {
    /// Reach `pulse` once every `every` units (at least 1).
    pub fn new(pulse: &'a dyn Pulse, every: u64) -> Self {
        if pulse.may_stop() || pulse.may_report() {
            Self {
                pulse: Some(pulse),
                pending: 0,
                every: if every == 0 { 1 } else { every },
            }
        } else {
            Self {
                pulse: None,
                pending: 0,
                every: u64::MAX,
            }
        }
    }

    /// Count `completed` finished units. Once `every` units are pending,
    /// report them and check for cancellation.
    #[inline]
    #[track_caller]
    pub fn step(&mut self, completed: u64) -> Result<(), StopReason> {
        self.pending = self.pending.saturating_add(completed);
        if self.pending < self.every {
            Ok(())
        } else {
            self.reach()
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
    #[track_caller]
    pub fn flush(&mut self) {
        if let Some(pulse) = self.pulse {
            if self.pending != 0 {
                pulse.advance(core::mem::take(&mut self.pending));
            }
        }
    }

    /// The slow path of [`step`](Self::step), kept out of the caller's loop.
    #[cold]
    #[inline(never)]
    #[track_caller]
    fn reach(&mut self) -> Result<(), StopReason> {
        match self.pulse {
            Some(pulse) => {
                pulse.advance(core::mem::take(&mut self.pending));
                pulse.check()
            }
            None => {
                self.pending = 0;
                Ok(())
            }
        }
    }
}

impl Drop for Paced<'_> {
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

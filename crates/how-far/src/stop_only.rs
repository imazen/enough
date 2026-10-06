//! A pulse made from a cancellation policy alone.
use crate::{Child, Execution, PhaseSpec, PlanError, Pulse, Report, SharedPulse, Total};
use alloc::{sync::Arc, vec::Vec};
use core::fmt;
use enough::{Stop, StopReason};

/// A stop policy held by value or by reference. Only an owned one can be shared.
#[derive(Clone)]
pub(crate) enum StopSource<'a> {
    Borrowed(&'a dyn Stop),
    Owned(Arc<dyn Stop>),
}
impl StopSource<'_> {
    pub(crate) fn get(&self) -> &dyn Stop {
        match self {
            Self::Borrowed(stop) => *stop,
            Self::Owned(stop) => stop.as_ref(),
        }
    }
    pub(crate) fn owned(&self) -> Result<Arc<dyn Stop>, PlanError> {
        match self {
            Self::Borrowed(_) => Err(PlanError::NotShareable),
            Self::Owned(stop) => Ok(stop.clone()),
        }
    }
}

/// Cancellation without progress: a pulse made from any [`Stop`].
///
/// For an application that holds a cancellation token, such as a `Stopper` or
/// a deadline, and calls a library that takes `&dyn Pulse`. Every phase the
/// library plans checks the same stop; reports go nowhere, and plans are only
/// validated. An owned stop can be [shared](Pulse::share) with spawned work; a
/// borrowed one serves scoped work and refuses to be shared.
///
/// ```
/// use how_far::{prelude::*, PhaseSpec, Stages, StopOnly, StopReason, Total};
///
/// // A library that knows nothing about the caller's token.
/// fn encode(rows: u64, pulse: &dyn Pulse) -> Result<(), StopReason> {
///     Stages::new(pulse, &[PhaseSpec::new("rows", 1, Total::Exact(rows))])
///         .complete_with(|stages| stages.run(|stage| {
///             for _ in 0..rows {
///                 stage.step(1)?;
///             }
///             Ok(())
///         }))
/// }
///
/// struct Cancelled;
/// impl Stop for Cancelled {
///     fn check(&self) -> Result<(), StopReason> {
///         Err(StopReason::Cancelled)
///     }
/// }
/// assert_eq!(encode(8, &StopOnly::new(Cancelled)), Err(StopReason::Cancelled));
/// assert_eq!(encode(8, &StopOnly::borrowed(&how_far::Unstoppable)), Ok(()));
/// ```
pub struct StopOnly<'a> {
    stop: StopSource<'a>,
}

impl StopOnly<'static> {
    /// Own `stop`; the pulse and its shared views can outlive the caller.
    pub fn new(stop: impl Stop + 'static) -> Self {
        Self {
            stop: StopSource::Owned(Arc::new(stop)),
        }
    }
}

impl<'a> StopOnly<'a> {
    /// Borrow `stop`, without allocating. [`Pulse::share`] then fails with
    /// [`PlanError::NotShareable`], since the view could outlive the borrow.
    pub fn borrowed(stop: &'a dyn Stop) -> Self {
        Self {
            stop: StopSource::Borrowed(stop),
        }
    }
}

impl fmt::Debug for StopOnly<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StopOnly")
            .field("owned", &matches!(self.stop, StopSource::Owned(_)))
            .finish()
    }
}

impl Stop for StopOnly<'_> {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.stop.get().check()
    }
    fn may_stop(&self) -> bool {
        self.stop.get().may_stop()
    }
}

impl Report for StopOnly<'_> {
    fn advance(&self, _: u64) {}
    fn may_report(&self) -> bool {
        false
    }
}

impl Pulse for StopOnly<'_> {
    fn split(&self, _: Execution, parts: &[PhaseSpec<'_>]) -> Result<Vec<Child<'_>>, PlanError> {
        PhaseSpec::validate_split(parts)?;
        // Untracked children check this token at every level and share
        // through it.
        Ok(crate::pulse::untracked_children(self, parts.len()))
    }
    /// Nobody observes the total, so a revision is accepted and discarded.
    fn set_total(&self, _: Total) -> Result<(), PlanError> {
        Ok(())
    }
    fn share(&self) -> Result<SharedPulse, PlanError> {
        Ok(SharedPulse::new(StopOnly {
            stop: StopSource::Owned(self.stop.owned()?),
        }))
    }
}

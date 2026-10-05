//! Erased cancellation adapters; no copy of their machinery per inner type.
use crate::{
    Child, ChildPulse, Execution, Outcome, PhaseSpec, PlanError, Pulse, Report, SharedPulse, Stop,
    StopReason, Total, stop_only::StopSource,
};
use alloc::{boxed::Box, sync::Arc, vec::Vec};

/// Adds or replaces a pulse's checkpoint policy, including through descendants.
///
/// Borrowed policies need no allocation and accept a local `&dyn Stop`. They
/// cannot outlive that borrow: [`Pulse::share`] returns [`PlanError::NotShareable`].
/// Use an owned policy with [`Self::new`] or [`Self::replacing`] for spawned workers.
/// Scoped workers can borrow this adapter directly.
///
/// Replacement bypasses the inner `check` entirely, including its callbacks.
/// Reporting, planning, ownership and completion still reach the inner pulse.
/// Combining is usually appropriate when adding a library-local cancellation source.
pub struct WithStop<'a> {
    inner: Inner<'a>,
    stop: StopSource<'a>,
    mode: Mode,
}
enum Inner<'a> {
    Borrowed(&'a dyn Pulse),
    Shared(SharedPulse),
    Child(Child<'a>),
}
#[derive(Clone, Copy)]
enum Mode {
    Combine,
    Replace,
}
impl<'a> WithStop<'a> {
    /// Check the inner policy, then `stop`. Both remain attached to descendants.
    pub fn new(inner: &'a dyn Pulse, stop: impl Stop + 'static) -> Self {
        Self {
            inner: Inner::Borrowed(inner),
            stop: StopSource::Owned(Arc::new(stop)),
            mode: Mode::Combine,
        }
    }
    /// Combine policies without owning or allocating the additional stop source.
    pub fn borrowed(inner: &'a dyn Pulse, stop: &'a dyn Stop) -> Self {
        Self {
            inner: Inner::Borrowed(inner),
            stop: StopSource::Borrowed(stop),
            mode: Mode::Combine,
        }
    }
    /// Use only `stop` for checkpoints, bypassing the inner stop and callbacks.
    pub fn replacing(inner: &'a dyn Pulse, stop: impl Stop + 'static) -> Self {
        Self {
            inner: Inner::Borrowed(inner),
            stop: StopSource::Owned(Arc::new(stop)),
            mode: Mode::Replace,
        }
    }
    /// Replace checkpoints with a borrowed policy; see [`Self::replacing`].
    pub fn replacing_borrowed(inner: &'a dyn Pulse, stop: &'a dyn Stop) -> Self {
        Self {
            inner: Inner::Borrowed(inner),
            stop: StopSource::Borrowed(stop),
            mode: Mode::Replace,
        }
    }
    fn inner(&self) -> &dyn Pulse {
        match &self.inner {
            Inner::Borrowed(p) => *p,
            Inner::Shared(p) => p.as_pulse(),
            Inner::Child(p) => p.pulse(),
        }
    }
}
impl Stop for WithStop<'_> {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        if matches!(self.mode, Mode::Combine) {
            self.inner().check()?;
        }
        self.stop.get().check()
    }
    fn may_stop(&self) -> bool {
        (matches!(self.mode, Mode::Combine) && self.inner().may_stop())
            || self.stop.get().may_stop()
    }
}
impl Report for WithStop<'_> {
    #[track_caller]
    fn advance(&self, n: u64) {
        self.inner().advance(n)
    }
    fn may_report(&self) -> bool {
        self.inner().may_report()
    }
}
impl Pulse for WithStop<'_> {
    #[track_caller]
    fn record_issue(&self, error: PlanError) {
        self.inner().record_issue(error);
    }
    fn split(&self, e: Execution, parts: &[PhaseSpec<'_>]) -> Result<Vec<Child<'_>>, PlanError> {
        let children = self.inner().split(e, parts)?;
        Ok(children
            .into_iter()
            .map(|child| {
                Child::new(WithStop {
                    inner: Inner::Child(child),
                    stop: self.stop.clone(),
                    mode: self.mode,
                })
            })
            .collect())
    }
    fn share(&self) -> Result<SharedPulse, PlanError> {
        let stop = self.stop.owned()?;
        Ok(SharedPulse::new(WithStop {
            inner: Inner::Shared(self.inner().share()?),
            stop: StopSource::Owned(stop),
            mode: self.mode,
        }))
    }
    fn start(&self) -> Result<(), PlanError> {
        self.inner().start()
    }
    fn set_total(&self, total: Total) -> Result<(), PlanError> {
        self.inner().set_total(total)
    }
}
impl ChildPulse for WithStop<'_> {
    fn complete_inferred(self: Box<Self>, outcome: Outcome) {
        if let Inner::Child(child) = self.inner {
            child.complete_inferred(outcome);
        }
    }
    fn complete_as(self: Box<Self>, outcome: Outcome) {
        if let Inner::Child(child) = self.inner {
            crate::Complete::complete_as(child, outcome);
        }
    }
    fn finish(self: Box<Self>, outcome: Outcome) -> Result<(), PlanError> {
        match self.inner {
            Inner::Child(child) => child.finish(outcome),
            _ => Err(PlanError::Unsupported),
        }
    }
}

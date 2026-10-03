//! One erased cancellation adapter; no copy of its machinery per inner type.
use crate::{
    Child, ChildPulse, Execution, NoPulse, Outcome, PhaseSpec, PlanError, ProgressWithStop, Pulse,
    PulseHandle, Report, SharedPulse, Stop, StopReason, Total,
};
use alloc::{boxed::Box, sync::Arc, vec::Vec};

/// Adds cancellation to a pulse, preserving it through children and sharing.
/// `stop_only` checks cancellation without allocating a tracking tree.
pub struct WithStop<'a> {
    inner: Inner<'a>,
    stop: Arc<dyn Stop>,
}
enum Inner<'a> {
    Borrowed(&'a dyn Pulse),
    Shared(SharedPulse),
    Child(Child<'a>),
}
impl<'a> WithStop<'a> {
    /// Check the inner policy, then `stop`. Both remain attached to descendants.
    pub fn new(inner: &'a dyn Pulse, stop: impl Stop + 'static) -> Self {
        Self {
            inner: Inner::Borrowed(inner),
            stop: Arc::new(stop),
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
impl WithStop<'static> {
    /// Cancellation only: validate plans, ignore progress, retain the stop policy.
    pub fn stop_only(stop: impl Stop + 'static) -> Self {
        Self::new(&NoPulse, stop)
    }
}
impl Stop for WithStop<'_> {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.inner().check()?;
        self.stop.check()
    }
    fn may_stop(&self) -> bool {
        self.inner().may_stop() || self.stop.may_stop()
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
    fn split(&self, e: Execution, parts: &[PhaseSpec<'_>]) -> Result<Vec<Child<'_>>, PlanError> {
        let children = self.inner().split(e, parts)?;
        let mut result = Vec::with_capacity(children.len());
        for child in children {
            result.push(Child::new(WithStop {
                inner: Inner::Child(child),
                stop: self.stop.clone(),
            }));
        }
        Ok(result)
    }
    fn handle(&self) -> PulseHandle {
        let handle = self.inner().handle();
        ProgressWithStop::new(
            Some(Arc::new(BothStop(handle.stop, self.stop.clone()))),
            handle.report,
        )
    }
    fn share(&self) -> Result<SharedPulse, PlanError> {
        Ok(SharedPulse::new(WithStop {
            inner: Inner::Shared(self.inner().share()?),
            stop: self.stop.clone(),
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
    fn finish(self: Box<Self>, outcome: Outcome) -> Result<(), PlanError> {
        match self.inner {
            Inner::Child(child) => child.finish(outcome),
            _ => Err(PlanError::Unsupported),
        }
    }
}
struct BothStop(Option<Arc<dyn Stop>>, Arc<dyn Stop>);
impl Stop for BothStop {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.0.check()?;
        self.1.check()
    }
    fn may_stop(&self) -> bool {
        self.0.may_stop() || self.1.may_stop()
    }
}

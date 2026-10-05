//! Transfer the final work result without converting or consuming its error.
use crate::{Outcome, StopReason};

/// Whether an error is a cancellation, and which one.
///
/// Progress owners use it to record a failed phase as `Cancelled` rather than
/// `Failed`; it never converts or consumes the error. [`StopReason`]
/// implements it. A library implements it for its own error type, next to the
/// `From<StopReason>` that lets `?` work at a checkpoint:
///
/// ```
/// use how_far::{IsStop, StopReason};
///
/// enum Error {
///     Corrupt,
///     Stopped(StopReason),
/// }
/// impl From<StopReason> for Error {
///     fn from(reason: StopReason) -> Self {
///         Self::Stopped(reason)
///     }
/// }
/// impl IsStop for Error {
///     fn stop_reason(&self) -> Option<StopReason> {
///         match self {
///             Self::Stopped(reason) => Some(*reason),
///             _ => None,
///         }
///     }
/// }
/// assert!(Error::Corrupt.stop_reason().is_none());
/// ```
///
/// A foreign error type the library cannot implement it for goes through the
/// `_classified` helpers with a closure instead.
///
/// Forgetting the implementation is reported as this trait being unimplemented,
/// not as a type mismatch elsewhere:
///
/// ```compile_fail,E0277
/// use how_far::{prelude::*, PhaseSpec, Stages, StopReason, Total};
///
/// enum Error {
///     Stopped(StopReason),
/// }
/// impl From<StopReason> for Error {
///     fn from(reason: StopReason) -> Self {
///         Self::Stopped(reason)
///     }
/// }
/// fn encode(pulse: &dyn Pulse) -> Result<(), Error> {
///     Stages::new(pulse, &[PhaseSpec::new("work", 1, Total::Unknown)])
///         .complete_with(|stages| stages.run(|stage| Ok(stage.check()?)))
/// }
/// ```
#[diagnostic::on_unimplemented(
    message = "`{Self}` does not say whether it is a cancellation",
    label = "progress tracking needs to classify this error",
    note = "implement `how_far::IsStop for {Self}`, returning `Some(reason)` for its stop variant",
    note = "for an error type you cannot implement it for, use `run_classified` or `complete_classified`"
)]
pub trait IsStop {
    /// The cancellation this error reports, or `None` for any other failure.
    fn stop_reason(&self) -> Option<StopReason>;
}

impl IsStop for StopReason {
    fn stop_reason(&self) -> Option<StopReason> {
        Some(*self)
    }
}

/// An owner that can record a result without changing the operation's result.
/// Reporting problems are diagnostic evidence, never replacement work errors.
pub trait Complete: Sized {
    /// Record an explicitly chosen outcome, resolving unfinished observations.
    /// Join workers before calling this. Started but uncompleted children are
    /// abandoned; untouched children are skipped on success, not run on error.
    fn complete_as(self, outcome: Outcome);

    /// Classify the error with [`IsStop`], record completion, and return the
    /// original result.
    fn complete<T, E>(self, result: Result<T, E>) -> Result<T, E>
    where
        E: IsStop,
    {
        self.complete_classified(result, |error| error.stop_reason().is_some())
    }

    /// Complete with an explicit classifier, for a foreign error type that
    /// Rust's orphan rules keep from implementing [`IsStop`].
    fn complete_classified<T, E>(
        self,
        result: Result<T, E>,
        is_stop: impl FnOnce(&E) -> bool,
    ) -> Result<T, E> {
        self.complete_as(Outcome::from_result(&result, is_stop));
        result
    }

    /// Run a multi-step `body` with this owner, then complete the owner with
    /// the body's result and return that result unchanged.
    ///
    /// An early `?` inside `body` still reaches the handoff, which a `?` placed
    /// before a separate `complete` call would bypass. A panic in `body` drops
    /// the owner, recording abandonment as usual.
    ///
    /// ```
    /// use how_far::{prelude::*, PhaseSpec, Stages, StopReason, Total};
    ///
    /// fn convert(pulse: &dyn Pulse) -> Result<u64, StopReason> {
    ///     Stages::new(pulse, &[
    ///         PhaseSpec::new("decode", 1, Total::Exact(1)),
    ///         PhaseSpec::new("sharpen", 1, Total::Exact(1)),
    ///     ])
    ///     .complete_with(|stages| {
    ///         let pixels = stages.run(|stage| stage.step(1).map(|()| 7))?;
    ///         // An untouched "sharpen" stage is resolved as Skipped.
    ///         Ok(pixels)
    ///     })
    /// }
    /// assert_eq!(convert(&how_far::NoPulse), Ok(7));
    /// ```
    fn complete_with<T, E>(mut self, body: impl FnOnce(&mut Self) -> Result<T, E>) -> Result<T, E>
    where
        E: IsStop,
    {
        let result = body(&mut self);
        self.complete(result)
    }
}

/// The explicit boundary where a normal Rust result completes its phase owner.
pub trait ResultExt<T, E>: Sized {
    /// Record completion and return this result unchanged. This is not invoked
    /// by `?`, `map`, or Drop automatically.
    fn finish_phase(self, owner: impl Complete) -> Result<T, E>
    where
        E: IsStop;
}
impl<T, E> ResultExt<T, E> for Result<T, E> {
    fn finish_phase(self, owner: impl Complete) -> Result<T, E>
    where
        E: IsStop,
    {
        owner.complete(self)
    }
}

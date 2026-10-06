//! Transfer the final work result without converting or consuming its error.
use crate::{IsStop, Outcome};

/// An owner that can record a result without changing the operation's result.
/// Reporting problems are diagnostic evidence, never replacement work errors.
pub trait Complete: Sized {
    /// Record an explicitly chosen outcome, resolving unfinished observations.
    /// Join workers before calling this. Started but uncompleted children are
    /// abandoned; untouched children are skipped on success, not run on error.
    fn complete_as(self, outcome: Outcome);

    /// Classify the error with [`IsStop`], record completion, and return the
    /// original result.
    ///
    /// Forgetting `IsStop` for a library's error is reported as that trait
    /// being unimplemented, not as a type mismatch elsewhere:
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

    /// [`complete_with`](Self::complete_with) with an explicit classifier, for
    /// a foreign error type that cannot implement [`IsStop`], such as an error
    /// wrapped with its source location.
    fn complete_with_classified<T, E>(
        mut self,
        is_stop: impl FnOnce(&E) -> bool,
        body: impl FnOnce(&mut Self) -> Result<T, E>,
    ) -> Result<T, E> {
        let result = body(&mut self);
        self.complete_classified(result, is_stop)
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

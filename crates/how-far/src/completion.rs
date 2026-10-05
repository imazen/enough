//! Transfer the final work result without converting or consuming its error.
use crate::{Outcome, StopReason};

/// An owner that can record a result without changing the operation's result.
/// Reporting problems are diagnostic evidence, never replacement work errors.
pub trait Complete: Sized {
    /// Record an explicitly chosen outcome, resolving unfinished observations.
    /// Join workers before calling this. Started but uncompleted children are
    /// abandoned; untouched children are skipped on success, not run on error.
    fn complete_as(self, outcome: Outcome);

    /// Classify a borrowed error, record completion, and return the original result.
    fn complete<T, E>(self, result: Result<T, E>) -> Result<T, E>
    where
        for<'a> &'a E: TryInto<StopReason>,
    {
        self.complete_classified(result, |error| error.try_into().is_ok())
    }

    /// Complete a foreign error wrapper using an explicit classifier when Rust's
    /// orphan rules prevent implementing the borrowed StopReason conversion.
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
        for<'a> &'a E: TryInto<StopReason>,
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
        for<'a> &'a E: TryInto<StopReason>;
}
impl<T, E> ResultExt<T, E> for Result<T, E> {
    fn finish_phase(self, owner: impl Complete) -> Result<T, E>
    where
        for<'a> &'a E: TryInto<StopReason>,
    {
        owner.complete(self)
    }
}

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

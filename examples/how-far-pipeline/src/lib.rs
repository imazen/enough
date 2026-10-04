//! A second library nests the codec without depending on a tracker or diagnostics.
#![no_std]
extern crate alloc;
use alloc::vec::Vec;
use how_far::{Execution, PhaseSpec, Phases, Stages, StopReason, Total, prelude::*};
use how_far_example_codec::{self as codec, Mode};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Codec(codec::Error),
    Stopped(StopReason),
    Invalid(&'static str),
}
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Codec(error) => write!(f, "decoding failed: {error}"),
            Self::Stopped(reason) => reason.fmt(f),
            Self::Invalid(message) => f.write_str(message),
        }
    }
}
impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Codec(error) => Some(error),
            _ => None,
        }
    }
}
impl From<codec::Error> for Error {
    fn from(error: codec::Error) -> Self {
        Self::Codec(error)
    }
}
impl From<StopReason> for Error {
    fn from(reason: StopReason) -> Self {
        Self::Stopped(reason)
    }
}
impl TryFrom<&Error> for StopReason {
    type Error = ();
    fn try_from(error: &Error) -> Result<Self, ()> {
        match error {
            Error::Stopped(reason) => Ok(*reason),
            Error::Codec(error) => Self::try_from(error),
            _ => Err(()),
        }
    }
}

/// Capture the entire body result before `?` can bypass the plan's final handoff.
pub fn convert(
    input: &[u8],
    pulse: &dyn Pulse,
    mode: Mode,
    sharpen: bool,
) -> Result<Vec<u8>, Error> {
    let mut stages = Stages::new(
        pulse,
        &[
            PhaseSpec::new("decode", 4, Total::Unknown),
            PhaseSpec::new("transform", 4, Total::Exact(input.len() as u64)),
            PhaseSpec::new("sharpen", 1, Total::Exact(1)),
        ],
    );
    let result = (|| {
        let mut pixels = stages.run(|p| codec::decode(input, p, mode).map_err(Error::from))?;
        stages.run(|p| {
            p.check()?;
            let mut paced = p.paced(64);
            for pixel in &mut pixels {
                *pixel ^= 0xff;
                paced.step(1)?;
            }
            paced.finish().map_err(Error::from)
        })?;
        if sharpen {
            stages.run(|p| p.step(1).map_err(Error::from))?;
        }
        Ok(pixels)
    })();
    result.finish_phase(stages)
}

/// Keep the caller's stage and cancellation, adding a library-local stop source.
/// Both are borrowed, so a stack-local token or caller-supplied `&dyn Stop` works.
pub fn convert_with_stop(
    input: &[u8],
    pulse: &dyn Pulse,
    additional_stop: &dyn Stop,
    mode: Mode,
    sharpen: bool,
) -> Result<Vec<u8>, Error> {
    let pulse = how_far::WithStop::borrowed(pulse, additional_stop);
    convert(input, &pulse, mode, sharpen)
}

/// Intentionally incorrect implementations, for the diagnostic demonstration.
#[derive(Clone, Copy, Debug)]
pub enum Bug {
    MissingHandoff,
    EarlyQuestionMark,
    MissingReports,
    SwallowedStop,
    MisclassifiedStop,
    DuplicateStage,
    CountBeforePlan,
    ForgottenRequiredStage,
    ReportToBranch,
}

pub fn buggy(pulse: &dyn Pulse, bug: Bug) -> Result<(), Error> {
    if matches!(bug, Bug::CountBeforePlan) {
        pulse.advance(1);
    }
    let mut phases = Phases::new(
        pulse,
        Execution::Sequence,
        &[
            PhaseSpec::new("work", 1, Total::Exact(2)),
            PhaseSpec::new("required-tail", 1, Total::Exact(1)),
        ],
    );
    if matches!(bug, Bug::ReportToBranch) {
        pulse.advance(1);
    }
    if matches!(bug, Bug::EarlyQuestionMark) {
        // The original error reaches the caller, but phases never sees it.
        phases.run(0, |_| Err::<(), _>(Error::Invalid("bad input")))?;
    }
    let result = phases.run(0, |p| {
        match bug {
            Bug::SwallowedStop => {
                let _ = p.check();
            }
            Bug::MisclassifiedStop => {
                p.check().map_err(|_| Error::Invalid("lost stop reason"))?;
            }
            _ => p.check()?,
        }
        if !matches!(bug, Bug::MissingReports) {
            p.advance(2);
        }
        Ok::<_, Error>(())
    });
    if matches!(bug, Bug::DuplicateStage) {
        phases.run(0, |p| p.check().map_err(Error::from))?;
    }
    if matches!(bug, Bug::MissingHandoff) {
        return result;
    }
    // ForgottenRequiredStage looks exactly like an intentional unused branch.
    result.finish_phase(phases)
}

/// Same work with and without the final partial-batch cancellation check.
pub fn final_batch(
    pulse: &dyn Pulse,
    request_stop: impl FnOnce(),
    finish: bool,
) -> Result<(), Error> {
    pulse.check()?;
    let mut paced = pulse.paced(64);
    paced.step(2)?;
    request_stop();
    if finish {
        paced.finish()?;
    }
    Ok(())
}

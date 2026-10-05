//! A no_std + alloc library with its own errors, fallback policy, and progress plan.
#![no_std]
extern crate alloc;
use alloc::vec::Vec;
use how_far::{Execution, PhaseSpec, Phases, StopReason, Total, prelude::*};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Unsupported,
    InvalidInput,
    Stopped(StopReason),
}
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unsupported => f.write_str("fast decoder does not support this input"),
            Self::InvalidInput => f.write_str("invalid encoded input"),
            Self::Stopped(reason) => reason.fmt(f),
        }
    }
}
impl core::error::Error for Error {}
impl From<StopReason> for Error {
    fn from(reason: StopReason) -> Self {
        Self::Stopped(reason)
    }
}
impl IsStop for Error {
    fn stop_reason(&self) -> Option<StopReason> {
        match self {
            Self::Stopped(reason) => Some(*reason),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Mode {
    Fast,
    Fallback,
    Fatal,
    FallbackFails,
}

fn copy(input: &[u8], pulse: &dyn Pulse) -> Result<Vec<u8>, Error> {
    pulse.check()?;
    let mut output = Vec::with_capacity(input.len());
    let mut paced = pulse.paced(64);
    for byte in input {
        output.push(*byte);
        paced.step(1)?;
    }
    paced.finish()?;
    Ok(output)
}

/// Only Unsupported is recoverable. Cancellation is never retried here.
pub fn decode(input: &[u8], pulse: &dyn Pulse, mode: Mode) -> Result<Vec<u8>, Error> {
    let mut attempts = Phases::new(
        pulse,
        Execution::Sequence,
        &[
            PhaseSpec::new("fast", 1, Total::Exact(input.len() as u64)),
            PhaseSpec::new("fallback", 1, Total::Exact(input.len() as u64)),
        ],
    );
    let first = attempts.run(0, |p| {
        p.check()?;
        match mode {
            Mode::Fast => copy(input, p),
            Mode::Fatal => Err(Error::InvalidInput),
            _ => Err(Error::Unsupported),
        }
    });
    let result = match first {
        Err(Error::Unsupported) => attempts.run(1, |p| {
            if matches!(mode, Mode::FallbackFails) {
                return Err(Error::InvalidInput);
            }
            copy(input, p)
        }),
        result => result,
    };
    result.finish_phase(attempts)
}

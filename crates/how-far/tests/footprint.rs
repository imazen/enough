//! What a caller holds and passes, in machine words.
//!
//! `&dyn Pulse` is a data pointer plus a vtable pointer however large the
//! implementation behind it is, so hot-path calls take two words of argument
//! (plus a `u64` for `advance`) and return at most one byte. These bounds hold
//! on 32- and 64-bit targets and are part of the interface: a change that
//! grows them should fail here first.

use how_far::{
    Child, Execution, Inert, NoReport, Outcome, Paced, PhaseSpec, PlanError, ProgressWithStop,
    Pulse, PulseHandle, Report, Stages, Stop, StopReason, Total, Unstoppable,
};
use std::mem::size_of;

const WORD: usize = size_of::<usize>();

// Compile-time, so a regression cannot slip past a skipped test.
const _: () = {
    assert!(size_of::<&dyn Pulse>() == 2 * WORD);
    assert!(size_of::<&dyn Stop>() == 2 * WORD);
    assert!(size_of::<&dyn Report>() == 2 * WORD);
    assert!(size_of::<Child<'static>>() == 2 * WORD);
    assert!(size_of::<Box<dyn Pulse>>() == 2 * WORD);
    // `ProgressExt::live` keeps the two words: `None` is the null pointer.
    assert!(size_of::<Option<&dyn Pulse>>() == 2 * WORD);
    // A pulse and two counters; its `step` keeps them in registers in a loop.
    assert!(size_of::<Paced<'static>>() == 2 * WORD + 16);
    assert!(size_of::<Result<(), StopReason>>() == 1);
    assert!(size_of::<Result<(), PlanError>>() == 1);
    assert!(size_of::<Outcome>() == 1);
    assert!(size_of::<PulseHandle>() == 4 * WORD);
    // One byte, so the one static `NoPulse` has an address of its own.
    assert!(size_of::<Inert>() == 1);
    assert!(size_of::<NoReport>() == 0);
    assert!(size_of::<how_far::SharedPulse>() == 2 * WORD);
    assert!(size_of::<ProgressWithStop<Unstoppable, NoReport>>() == 0);
};

#[test]
fn hot_path_values_fit_in_registers() {
    // Two words in, at most one byte out: no stack spill at the call itself.
    assert_eq!(size_of::<&dyn Pulse>(), 2 * WORD);
    assert_eq!(size_of::<Result<(), StopReason>>(), 1);
    assert_eq!(size_of::<Child<'static>>(), 2 * WORD);
}

#[test]
fn cold_path_values_have_known_sizes() {
    // Plans are passed as slices, so a split is two words however many parts.
    assert_eq!(size_of::<&[PhaseSpec<'static>]>(), 2 * WORD);
    assert!(size_of::<PhaseSpec<'static>>() <= 10 * size_of::<u64>());
    assert!(size_of::<Total>() <= 2 * size_of::<u64>());
    assert!(size_of::<Execution>() <= 2 * WORD);
    assert!(size_of::<Stages<'static>>() <= 7 * WORD);
    #[cfg(feature = "checked")]
    {
        assert!(size_of::<how_far::TryStages<'static>>() <= 5 * WORD);
        assert!(size_of::<how_far::RunError<StopReason>>() <= 2);
    }
}

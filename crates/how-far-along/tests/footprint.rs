//! Handle sizes in the tracker, in machine words. Cloned handles are single
//! `Arc`s, so giving one to every worker costs one word and a reference count.

use how_far_along::ext::Batch;
use how_far_along::{NodeId, Observer, Phase, PulseTree, Reporter};
use std::mem::size_of;

const WORD: usize = size_of::<usize>();

const _: () = {
    assert!(size_of::<Reporter>() == WORD);
    assert!(size_of::<Observer>() == WORD);
    assert!(size_of::<Phase>() == WORD);
    assert!(size_of::<NodeId>() == WORD);
    assert!(size_of::<Option<NodeId>>() == 2 * WORD);
    assert!(size_of::<Batch<Reporter>>() <= WORD + 2 * size_of::<u64>());
};

#[test]
fn the_tree_behind_dyn_pulse_never_reaches_a_callers_signature() {
    // Callers pass `&tree` as `&dyn Pulse`: two words, whatever this is.
    assert!(size_of::<PulseTree>() <= 16 * WORD);
}

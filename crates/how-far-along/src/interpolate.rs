//! Progress between reports, estimated on the polling side.
//!
//! A library reports at its own granularity, and should not be asked to
//! report more often only so that a display moves smoothly. A display polled
//! faster than the reports sees the fraction jump at each one and stand still
//! between them. [`Interpolator`] smooths that without touching the library:
//! it remembers, for each running leaf, when it first saw each new count and
//! how far apart the counts have come, and between reports advances the leaf
//! at that pace.
//!
//! It assumes that a phase's reports are roughly evenly spaced, as they are
//! for a loop over rows, tiles or chunks. Each phase keeps its own pace, so
//! a fast stage followed by a slow one is not extrapolated at the fast pace.
//! An estimate never passes the next expected report or the phase's total,
//! and never goes backwards. A leaf that has not yet been seen to change has
//! no pace and shows its count.
//!
//! The tracker reads no clock, so the caller passes the time. The result is
//! for display; decisions should use the snapshot's own fraction.
//!
//! ```
//! use core::time::Duration;
//! use how_far_along::{Phase, Report, Total, interpolate::Interpolator};
//!
//! let job = Phase::new("decode", Total::Exact(10));
//! let reporter = job.reporter();
//! let mut smooth = Interpolator::new();
//! let at = |ms| Duration::from_millis(ms);
//!
//! smooth.fraction(&job.observer().snapshot(), at(0));
//! reporter.advance(1); // one row every 100 ms
//! smooth.fraction(&job.observer().snapshot(), at(100));
//! reporter.advance(1);
//! smooth.fraction(&job.observer().snapshot(), at(200));
//! // Half-way to the next row, the display is half-way there too.
//! let shown = smooth.fraction(&job.observer().snapshot(), at(250)).unwrap();
//! assert!((shown - 0.25).abs() < 1e-9);
//! // A late row holds the display at the next expected count.
//! let shown = smooth.fraction(&job.observer().snapshot(), at(900)).unwrap();
//! assert!((shown - 0.3).abs() < 1e-9);
//! ```

use crate::{NodeId, Snapshot, Status, Total};
use alloc::vec::Vec;
use core::time::Duration;

/// Smooths a polled tree's fraction between reports; see the
/// [module docs](self). Keep one per tracked operation and pass it every
/// snapshot you display.
#[derive(Default)]
pub struct Interpolator {
    leaves: Vec<Pace>,
    shown: f64,
}

impl core::fmt::Debug for Interpolator {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Interpolator")
            .field("running_leaves", &self.leaves.len())
            .field("shown", &self.shown)
            .finish()
    }
}

/// What one running leaf's reports have looked like so far.
struct Pace {
    node: NodeId,
    /// The latest count, and when it was first seen.
    count: u64,
    since: Duration,
    /// Units per change and seconds per change, averaged over the changes seen.
    step: f64,
    interval: f64,
    changes: u32,
    /// The largest count shown, so a leaf's display never goes back.
    shown: f64,
    /// Seen in the current call; leaves no longer running are dropped.
    seen: bool,
}

impl Pace {
    fn new(node: NodeId, count: u64, now: Duration) -> Self {
        Self {
            node,
            count,
            since: now,
            step: 0.0,
            interval: 0.0,
            changes: 0,
            shown: count as f64,
            seen: true,
        }
    }

    /// Take a new count, then estimate the count at `now`.
    fn estimate(&mut self, count: u64, total: u64, now: Duration) -> f64 {
        if count > self.count {
            let step = (count - self.count) as f64;
            let interval = now.saturating_sub(self.since).as_secs_f64();
            // The first change sets the pace; later ones move it a quarter of the way.
            let weight = if self.changes == 0 { 1.0 } else { 0.25 };
            self.step += (step - self.step) * weight;
            self.interval += (interval - self.interval) * weight;
            self.changes = self.changes.saturating_add(1);
            self.count = count;
            self.since = now;
        }
        let mut estimate = self.count as f64;
        if self.changes != 0 && self.interval > 0.0 {
            let elapsed = now.saturating_sub(self.since).as_secs_f64();
            // Never past the next expected report.
            estimate += (self.step * elapsed / self.interval).min(self.step);
        }
        self.shown = self.shown.max(estimate.min(total as f64)).max(count as f64);
        self.shown
    }
}

impl Interpolator {
    /// An interpolator that has seen no reports.
    pub fn new() -> Self {
        Self::default()
    }

    /// `snapshot`'s fraction at `now`, with each running leaf advanced at
    /// the pace of its reports so far; `None` when the snapshot's own
    /// fraction is.
    ///
    /// `now` comes from any monotonic clock of the caller's, as a time since
    /// an epoch of its choosing; pass the same clock on every call. The
    /// result never goes backwards, even when a revised total moves the
    /// snapshot's own fraction back: it holds until the work catches up.
    pub fn fraction(&mut self, snapshot: &Snapshot, now: Duration) -> Option<f64> {
        for pace in &mut self.leaves {
            pace.seen = false;
        }
        let estimate = self.node(snapshot, now);
        // Forget leaves that are no longer running.
        let mut index = 0;
        while index < self.leaves.len() {
            if self.leaves[index].seen {
                index += 1;
            } else {
                self.leaves.swap_remove(index);
            }
        }
        self.shown = self.shown.max(estimate?);
        Some(self.shown)
    }

    /// [`Snapshot::fraction`], with running leaves estimated.
    fn node(&mut self, node: &Snapshot, now: Duration) -> Option<f64> {
        if node.children.is_empty() {
            return match (node.status, node.total) {
                (Status::Running, Total::Exact(total) | Total::Estimated(total))
                    if total != 0 && !node.overflowed =>
                {
                    Some(self.leaf(node.id, node.completed, total, now) / total as f64)
                }
                _ => node.fraction(),
            };
        }
        if let Status::Finished(_) = node.status {
            return node.fraction();
        }
        let mut sum = 0.0;
        for child in &node.children {
            sum += child.weight as f64;
        }
        let (mut result, mut known) = (0.0, true);
        // Visit every child, so that no running leaf loses its pace.
        for child in &node.children {
            match self.node(child, now) {
                Some(fraction) => result += child.weight as f64 / sum * fraction,
                None => known = false,
            }
        }
        known.then_some(result.clamp(0.0, 1.0))
    }

    fn leaf(&mut self, node: NodeId, count: u64, total: u64, now: Duration) -> f64 {
        let mut index = 0;
        while index < self.leaves.len() && self.leaves[index].node != node {
            index += 1;
        }
        if index == self.leaves.len() {
            self.leaves.push(Pace::new(node, count, now));
        }
        let pace = &mut self.leaves[index];
        pace.seen = true;
        pace.estimate(count, total, now)
    }
}

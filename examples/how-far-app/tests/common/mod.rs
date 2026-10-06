//! Helpers shared by the scenario tests; each test binary uses a subset.
#![allow(dead_code)]

use how_far_along::{Observer, Phase, PulseTree, Snapshot, Stop, StopReason, Total};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};

/// A tracked root for one operation.
pub fn tree(name: &str, stop: impl Stop + 'static) -> PulseTree {
    PulseTree::new(Phase::new(name, Total::Unknown), stop)
}

/// A tree whose stop policy trips the first time `rule` holds for a snapshot
/// taken at a checkpoint. It makes cancellation land at an exact point in a
/// library's work, on whichever thread reaches that point first.
pub fn tree_stopping_when(
    name: &str,
    rule: impl Fn(&Snapshot) -> bool + Send + Sync + 'static,
) -> PulseTree {
    let observer = Arc::new(OnceLock::new());
    let tree = tree(
        name,
        StopWhen {
            observer: observer.clone(),
            rule: Box::new(rule),
            tripped: AtomicBool::new(false),
        },
    );
    observer
        .set(tree.observer())
        .unwrap_or_else(|_| unreachable!("set once"));
    tree
}

type Rule = Box<dyn Fn(&Snapshot) -> bool + Send + Sync>;

struct StopWhen {
    observer: Arc<OnceLock<Observer>>,
    rule: Rule,
    tripped: AtomicBool,
}

impl Stop for StopWhen {
    fn check(&self) -> Result<(), StopReason> {
        if self.tripped.load(Ordering::Acquire) {
            return Err(StopReason::Cancelled);
        }
        if let Some(observer) = self.observer.get()
            && (self.rule)(&observer.snapshot())
        {
            self.tripped.store(true, Ordering::Release);
            return Err(StopReason::Cancelled);
        }
        Ok(())
    }
}

/// Find a phase by its path of names below `root`, such as
/// `["encode", "image 1", "entropy"]`.
pub fn find<'a>(root: &'a Snapshot, path: &[&str]) -> &'a Snapshot {
    path.iter().fold(root, |node, name| {
        node.children
            .iter()
            .find(|child| child.name == *name)
            .unwrap_or_else(|| panic!("no phase {name:?} under {:?}", node.name))
    })
}

/// Every phase in the tree, depth first.
pub fn all(root: &Snapshot) -> Vec<&Snapshot> {
    let mut out = vec![root];
    for child in &root.children {
        out.extend(all(child));
    }
    out
}

/// A batch of small test images.
pub fn images(count: usize) -> Vec<how_far_example_codec::Image> {
    (0..count)
        .map(|i| how_far_example_codec::Image::pattern(64 + 16 * i, 48 + 8 * i))
        .collect()
}

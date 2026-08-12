//! Built off-thread by diffing two manifests in the document crate, and only ever *consumed* here:
//! the render side applies the table and never reasons about which nodes survived or why. It sits
//! on this side of the seam because it is a field of [`InstallBundle`](super::InstallBundle).
//!
//! see rules: execution-runtime

use alloc::vec::Vec;

/// The precomputed **migration table**: the `(old index, new index)` survivor pairs
/// the render side transplants by box swap. The survivor semantics — which nodes survive, how
/// indices map — stay off-thread with the Coordinator that built it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MigrationTable {
    survivors: Vec<(usize, usize)>,
}

impl MigrationTable {
    /// The pairing rule is the builder's; this side takes the result as given.
    pub fn new(survivors: Vec<(usize, usize)>) -> Self {
        Self { survivors }
    }

    /// The empty table (no survivors) — every node resets. The retiree posted back through the
    /// mailbox carries this: a reclaimed Engine has no migration to apply.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The `(old index, new index)` survivor pairs, for the transplant loop.
    pub fn survivors(&self) -> &[(usize, usize)] {
        &self.survivors
    }

    /// Number of survivor pairs.
    pub fn len(&self) -> usize {
        self.survivors.len()
    }

    /// Whether the table is empty (no survivors).
    pub fn is_empty(&self) -> bool {
        self.survivors.is_empty()
    }
}

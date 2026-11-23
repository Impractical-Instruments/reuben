//! Built off-thread by diffing two manifests in the document crate, and only ever *consumed* here:
//! the render side applies the table and never reasons about which nodes survived or why. It sits
//! on this side of the seam because it is a field of [`InstallBundle`](super::InstallBundle).

use alloc::vec::Vec;

use crate::plan::LiveCarry;

/// The precomputed **migration table**: the `(old index, new index)` survivor pairs the render
/// side transplants by box swap, and the live input values it carries across. The survivor
/// semantics — which nodes survive, which live values they keep, how indices map — stay off-thread
/// with the Coordinator that built it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MigrationTable {
    survivors: Vec<(usize, usize)>,
    live_values: Vec<LiveCarry>,
}

impl MigrationTable {
    /// The pairing and carry rules are the builder's; this side takes the result as given.
    pub fn new(survivors: Vec<(usize, usize)>, live_values: Vec<LiveCarry>) -> Self {
        Self {
            survivors,
            live_values,
        }
    }

    /// The empty table (no survivors) — every node resets. The retiree posted back through the
    /// mailbox carries this: a reclaimed Engine has no migration to apply.
    pub fn empty() -> Self {
        Self::default()
    }

    /// This table with its survivor pairs and no live values: every input starts from the new
    /// Engine's seed.
    pub fn without_live_values(self) -> Self {
        Self {
            survivors: self.survivors,
            live_values: Vec::new(),
        }
    }

    /// The `(old index, new index)` survivor pairs, for the transplant loop.
    pub fn survivors(&self) -> &[(usize, usize)] {
        &self.survivors
    }

    /// The live values that carry into the fresh Engine instead of its seeds. Each place appears
    /// at most once per side.
    pub fn live_values(&self) -> &[LiveCarry] {
        &self.live_values
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

//! `subpatch` — a nested instrument referenced as a node (nesting P4).
//!
//! A `subpatch` node names another instrument patch (an instrument-resource, via a
//! `patch` resource slot). At build the referenced patch is loaded recursively and **inlined**
//! (§2): its nodes are spliced into the parent graph under the subpatch's address prefix, parent
//! wires resolve through the boundary face synthesized from the child's `interface` (§4), and the
//! node **dissolves** — no `subpatch` ever reaches the built [`Graph`](crate::graph::Graph), the
//! `Plan`, or the renderer. That is the split: the Voicer **hosts** its voice
//! sub-patches at runtime (runtime-varying cardinality), whereas a static nest inlines at build
//! (fixed cardinality) for zero runtime cost (see rules: composition-operators).
//!
//! This registered operator is therefore a *format anchor*, not a DSP unit: it exists so `type`
//! keeps its "registered operator" invariant (§1) and so the registry/schema/introspection know
//! the node form. It declares **no ports** — the boundary face is synthesized per reference at
//! load, never registered here — and its [`process`](Operator::process) is an unreachable no-op
//! (the graph API could still instantiate one by hand; it renders nothing).
//!
//! - resource `patch` — the referenced instrument patch (instrument-resource).

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract: the only surface a `subpatch` declares is its `patch` resource
// slot — the third `(slot, ref)` entry alongside `sample`/`voice`. No inputs/outputs: the
// boundary face is synthesized from the referenced patch's `interface`, not registered here (§1, §4).
crate::operator_contract!(Subpatch {
    resources: { patch },
});

/// A reference to a nested instrument. Carries no runtime state: the loader inlines the referenced
/// patch and dissolves the node at build, so this operator is an inert format anchor
/// that never renders (see the module docs).
#[derive(Default)]
pub struct Subpatch;

impl Subpatch {
    pub fn new() -> Self {
        Self
    }
}

impl Operator for Subpatch {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    /// No-op: a `subpatch` dissolves at build and never survives to render; one
    /// instantiated by hand through the graph API simply produces nothing.
    fn process(&mut self, _io: &mut Io) {}

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Subpatch);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declares_only_the_patch_resource_slot() {
        let d = Subpatch::descriptor();
        assert_eq!(d.type_name, "subpatch");
        // The boundary face is synthesized from the child interface (P4+), so no ports are registered.
        assert!(d.inputs.is_empty(), "subpatch registers no input ports");
        assert!(d.outputs.is_empty(), "subpatch registers no output ports");
        assert!(d.constants.is_empty(), "subpatch registers no constants");
        // The one declared surface: the `patch` instrument-resource slot.
        assert!(d.has_resource("patch"), "subpatch declares a `patch` slot");
        assert!(!d.has_resource("sample"), "no `sample` slot");
        assert!(!d.has_resource("voice"), "no `voice` slot");
    }
}

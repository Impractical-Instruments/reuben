//! `subpatch` — a nested instrument referenced as a node (ADR-0034, nesting P3).
//!
//! A `subpatch` node names another instrument patch (an instrument-resource, ADR-0032 §2, via a
//! `patch` resource slot) and the loader carries the built sub-[`Graph`] on the **parent node**
//! (`Node::subpatch`, `graph.rs`) — *not* on this operator. That placement is the whole point of
//! the split ADR-0034 draws: the Voicer **hosts** its voice sub-patches at runtime (their graphs
//! ride in the operator, turned into per-voice sub-plans), whereas a static `subpatch` is destined
//! to be **inlined / dissolved** into the parent graph at plan-build (P4, [#119]). So the sub-graph
//! is build-time data on the node, consumed by that later inline pass — it never becomes runtime
//! state of a `subpatch` operator, and this operator never renders anything.
//!
//! This is P3 ([#118]): *reference and load only*. The node's ports are **synthesized** from the
//! resolved child's `interface` (§4) at inline/introspection time (P4–P6); until then this operator
//! declares **no ports** — only the `patch` resource slot — and its [`process`](Operator::process)
//! is a no-op. A `subpatch` node that survives to render (i.e. was never inlined) simply does
//! nothing, which is why P3 is safe to land before the inline pass exists.
//!
//! - resource `patch` — the referenced instrument patch (instrument-resource, ADR-0032 §2).
//!
//! [#118]: https://github.com/Impractical-Instruments/reuben/issues/118
//! [#119]: https://github.com/Impractical-Instruments/reuben/issues/119

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025): the only surface a `subpatch` declares is its `patch` resource
// slot — the third `(slot, ref)` entry alongside `sample`/`voice` (ADR-0034). No inputs/outputs: the
// boundary face is synthesized from the referenced patch's `interface`, not registered here (§1, §4).
crate::operator_contract!(Subpatch {
    resources: { patch },
});

/// A reference to a nested instrument. Carries no runtime state: the resolved sub-graph lives on the
/// parent [`Node`](crate::graph::Node), destined for plan-build inlining (P4), so this operator is an
/// inert placeholder that never renders (see the module docs).
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

    /// No-op: a `subpatch` is inlined at plan-build (P4) and never survives to render. An
    /// un-inlined one (P3, before the inline pass exists) simply produces nothing.
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
        // The one declared surface: the `patch` instrument-resource slot (ADR-0034).
        assert!(d.has_resource("patch"), "subpatch declares a `patch` slot");
        assert!(!d.has_resource("sample"), "no `sample` slot");
        assert!(!d.has_resource("voice"), "no `voice` slot");
    }
}

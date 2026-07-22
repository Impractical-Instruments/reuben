//! The engine-facing link the five engine tools drive.
//!
//! **One plane, not two.** Structure edits and control both ride the **loopback-only** TCP/NDJSON
//! structure channel ([`StructureClient`], #315). Control used to ride OSC/UDP instead — `send`
//! encoded datagrams and dispatched them to the endpoint the engine binds on all interfaces — which
//! meant the sidecar owned an OSC wire format for talking to its own peer. It no longer does: both
//! ends already speak core's types, and routing converges in core at `Engine::queue_osc` either way,
//! so `send` now ships `{address, [Arg]}` in this channel's own framing. OSC-the-binary-protocol is
//! the engine's **foreign** edge (external controllers in, `osc_out` nodes out), not an internal
//! hop.
//!
//! There is no engine-facing trait. The injectable seam is one layer down —
//! [`StructureTransport`](crate::StructureTransport), the socket itself — so tool-body tests
//! exercise real NDJSON serialization, real parsing, and the real unreachable classification
//! instead of a hand-written stand-in for them.
//!
//! # Act-then-map, not probe-then-act
//!
//! Every engine tool runs its real exchange and maps [`StructureError::is_unreachable`] to the
//! fail-fast [`crate::engine_unreachable`] result — one connection, no TOCTOU window between a
//! separate liveness probe and the act. `send` was the one exception, probing first because UDP is
//! silent about a dead port; riding TCP, its own exchange reports the dead engine, so the probe is
//! gone and the rule has no exceptions left.
//!
//! see rules: agent-mcp

use reuben_core::coordinator::DEFAULT_STRUCTURE_ADDR;

use crate::client::StructureClient;

/// The engine link: a handle to the one channel every engine tool speaks.
///
/// A thin named wrapper rather than an abstraction — nothing here forwards, and there is no trait.
/// It survives as a distinct type because the composition root injects it and `engine_status`
/// reports its endpoint; the injectable seam lives one layer down, at
/// [`StructureTransport`](crate::StructureTransport).
///
/// Cheap to hold: each exchange opens its own short-lived connection, so nothing is retained
/// between calls and the link survives the engine restarting under it.
#[derive(Debug)]
pub struct EngineLink {
    structure: StructureClient,
}

impl EngineLink {
    /// A link dialing `structure_addr` for the structure channel.
    pub fn new(structure_addr: impl Into<String>) -> Self {
        Self::from_client(StructureClient::new(structure_addr))
    }

    /// A link over an already-built [`StructureClient`] — the injection point for tests, which pair
    /// it with a fake transport.
    pub fn from_client(structure: StructureClient) -> Self {
        Self { structure }
    }

    /// The structure channel — liveness, `send`, `swap`, `get_document`, `get_diagnostics`.
    pub fn structure(&self) -> &StructureClient {
        &self.structure
    }

    /// The structure-channel endpoint address, for `engine_status`.
    pub fn structure_endpoint(&self) -> String {
        self.structure.addr().to_string()
    }
}

impl Default for EngineLink {
    /// A link targeting the shared [`DEFAULT_STRUCTURE_ADDR`] — the same address `reuben play`
    /// binds, so the sidecar and engine can never drift.
    fn default() -> Self {
        Self::new(DEFAULT_STRUCTURE_ADDR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_link_dials_exactly_what_reuben_play_binds() {
        // Sidecar and engine share one address const, so they cannot drift. Asserted on the REAL
        // construction path — `ReubenServer::new` builds this link — rather than on a
        // `StructureClient::default` no production code ever called (#493).
        let link = EngineLink::default();
        assert_eq!(link.structure_endpoint(), DEFAULT_STRUCTURE_ADDR);
        assert!(
            link.structure_endpoint().starts_with("127.0.0.1:"),
            "the structure channel must stay loopback-only: {}",
            link.structure_endpoint()
        );
    }
}

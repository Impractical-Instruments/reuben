//! The engine-facing link the five engine tools drive: the one loopback structure channel carrying
//! both structure edits and control.
//!
//! see rules: agent-mcp

use reuben_core::coordinator::DEFAULT_STRUCTURE_ADDR;

use crate::client::StructureClient;

/// The engine link: a handle to the one channel every engine tool speaks.
///
/// Cheap to hold — each exchange opens its own short-lived connection, so nothing is retained
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
        // Asserted on the REAL construction path — the one `ReubenServer::new` builds.
        let link = EngineLink::default();
        assert_eq!(link.structure_endpoint(), DEFAULT_STRUCTURE_ADDR);
        assert!(
            link.structure_endpoint().starts_with("127.0.0.1:"),
            "the structure channel must stay loopback-only: {}",
            link.structure_endpoint()
        );
    }
}

//! The engine-facing link the five engine tools drive.
//!
//! The window owns the channel and the loopback TCP transport under it; what is left here is which
//! address this door dials.

use reuben_api::engine::{Channel, TcpTransport, DEFAULT_CONNECT_TIMEOUT, DEFAULT_STRUCTURE_ADDR};

/// The engine link: a handle to the one channel every engine tool speaks.
#[derive(Debug)]
pub struct EngineLink {
    structure: Channel,
}

impl EngineLink {
    /// A link dialing `structure_addr` over loopback TCP with this door's connect budget.
    pub fn new(structure_addr: impl Into<String>) -> Self {
        Self::from_channel(Channel::new(TcpTransport::new(
            structure_addr,
            DEFAULT_CONNECT_TIMEOUT,
        )))
    }

    /// A link over an already-built channel — the injection point for tests, which pair it with a
    /// fake transport.
    pub fn from_channel(structure: Channel) -> Self {
        Self { structure }
    }

    /// The structure channel — liveness, `send`, `swap`, `get_document`, `get_diagnostics`.
    pub fn structure(&self) -> &Channel {
        &self.structure
    }

    /// The structure-channel endpoint address, for `engine_status`.
    pub fn structure_endpoint(&self) -> String {
        self.structure.endpoint().to_string()
    }
}

impl Default for EngineLink {
    /// A link targeting the shared default address — the same one `reuben play` binds, so the
    /// sidecar and engine can never drift.
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

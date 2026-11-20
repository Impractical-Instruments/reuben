//! The engine-facing link every tool but `describe_operators` drives.
//!
//! The window owns the channel and the loopback TCP transport under it; what is left here is which
//! address this door dials.

use std::fmt;
use std::net::SocketAddr;

use reuben_api::engine::{Channel, TcpTransport, DEFAULT_CONNECT_TIMEOUT, DEFAULT_STRUCTURE_ADDR};

/// The engine link: a handle to the one channel every tool that reads the engine or a file speaks.
#[derive(Debug)]
pub struct EngineLink {
    structure: Channel,
}

impl EngineLink {
    /// A link dialing `structure_addr` over loopback TCP with this door's connect budget.
    ///
    /// Refuses anything but a literal loopback `ip:port`, as `reuben play --structure-addr` does:
    /// the channel carries no authentication, so it must not leave the machine.
    pub fn new(structure_addr: impl Into<String>) -> Result<Self, NotLoopback> {
        let structure_addr = structure_addr.into();
        // Parsed, never resolved: a host name could resolve to a routable interface.
        let loopback = structure_addr
            .parse::<SocketAddr>()
            .is_ok_and(|parsed| parsed.ip().is_loopback());
        if !loopback {
            return Err(NotLoopback(structure_addr));
        }
        Ok(Self::from_channel(Channel::new(TcpTransport::new(
            structure_addr,
            DEFAULT_CONNECT_TIMEOUT,
        ))))
    }

    /// A link over an already-built channel — the injection point for tests, which pair it with a
    /// fake transport.
    pub fn from_channel(structure: Channel) -> Self {
        Self { structure }
    }

    /// The structure channel: the engine requests, and `call` for every document read and edit.
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
        Self::new(DEFAULT_STRUCTURE_ADDR).expect("the default structure address is loopback")
    }
}

/// A structure address [`EngineLink::new`] refused: not a literal loopback IP and port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotLoopback(pub String);

impl fmt::Display for NotLoopback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the structure address `{}` is not a loopback address; it must be a loopback \
             ip:port until token authentication exists",
            self.0
        )
    }
}

impl std::error::Error for NotLoopback {}

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

    #[test]
    fn a_loopback_address_is_dialed_as_given() {
        for addr in ["127.0.0.1:9200", "[::1]:9200"] {
            let link = EngineLink::new(addr).expect("loopback is accepted");
            assert_eq!(link.structure_endpoint(), addr);
        }
    }

    #[test]
    fn anything_but_a_literal_loopback_address_is_refused() {
        for addr in [
            "0.0.0.0:9124",
            "192.168.1.4:9124",
            "localhost:9124",
            "127.0.0.1",
            "",
        ] {
            assert_eq!(
                EngineLink::new(addr).expect_err("refused").0,
                addr,
                "{addr:?} must be refused"
            );
        }
    }
}

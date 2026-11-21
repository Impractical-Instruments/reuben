//! The engine-facing link every tool but `describe_operators` drives.
//!
//! The window owns the channel and the TCP transport under it; what is left here is which address
//! this door dials, and with which token.

use reuben_api::engine::{
    Channel, TcpTransport, Token, DEFAULT_CONNECT_TIMEOUT, DEFAULT_STRUCTURE_ADDR,
};

/// The engine link: a handle to the one channel every tool that reads the engine or a file speaks.
#[derive(Debug)]
pub struct EngineLink {
    structure: Channel,
}

impl EngineLink {
    /// A link dialing `structure_addr` over TCP with this door's connect budget, opening each
    /// connection with `token` when given. A server on another machine serves nothing without it.
    pub fn new(structure_addr: impl Into<String>, token: Option<Token>) -> Self {
        Self::from_channel(Channel::new(
            TcpTransport::new(structure_addr, DEFAULT_CONNECT_TIMEOUT).with_token(token),
        ))
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
        Self::new(DEFAULT_STRUCTURE_ADDR, None)
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

    #[test]
    fn any_address_is_dialed_as_given() {
        for addr in [
            "127.0.0.1:9200",
            "[::1]:9200",
            "192.168.1.4:9124",
            "studio.tail:9124",
        ] {
            let link = EngineLink::new(addr, None);
            assert_eq!(link.structure_endpoint(), addr);
        }
    }

    #[test]
    fn a_token_link_sends_the_credential_first() {
        use std::io::{BufRead, BufReader, Write};

        use reuben_api::engine::{credential_line, Request, Response};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(&stream);
            let (mut first, mut second) = (String::new(), String::new());
            reader.read_line(&mut first).expect("first");
            reader.read_line(&mut second).expect("second");
            (&stream)
                .write_all(Response::Pong.to_ndjson().as_bytes())
                .expect("answer");
            (first, second)
        });
        let token = Token::new("s3cret").expect("a token");
        let link = EngineLink::new(addr, Some(token.clone()));
        link.structure().ping().expect("answered");
        let (first, second) = server.join().expect("server");
        assert_eq!(first, credential_line(&token));
        assert_eq!(second, Request::Ping.to_ndjson());
    }
}

//! The engine-facing seam the five engine tools drive and its shipping implementation.
//!
//! Every engine tool reaches a user-owned `reuben play` through ONE seam, [`EngineChannel`], so
//! the tool bodies stay engine-independent and the unit tests inject an in-memory fake instead of
//! standing up a live engine. The shipping implementation is [`EngineLink`]: the structure-channel
//! [`StructureClient`] (loopback TCP/NDJSON, #315) for the three verbs plus liveness, and a UDP
//! socket to the engine's OSC-in endpoint for `send`.
//!
//! # Act-then-map, not probe-then-act
//!
//! The structure-channel tools (`swap`, `get_current_instrument`, `get_diagnostics`) run their
//! real exchange and map [`StructureError::is_unreachable`] to the fail-fast
//! [`crate::engine_unreachable`] result — one connection, no TOCTOU window between a separate
//! liveness probe and the act. `send` is the one probe-first tool: UDP is silent
//! about a dead port, so it pings the structure channel before dispatching datagrams.
//!
//! see rules: agent-mcp

use std::net::UdpSocket;

use reuben_core::coordinator::{
    DiagnosticsReport, DocSource, DEFAULT_OSC_PORT, DEFAULT_STRUCTURE_ADDR,
};

use crate::client::{DocumentSnapshot, StructureClient, StructureError, SwapOutcome};

/// The engine's OSC-in endpoint the sidecar dispatches `send` datagrams to, composed from the
/// shared [`DEFAULT_OSC_PORT`]. `reuben play` binds OSC-in on `0.0.0.0:<port>`; the sidecar and
/// engine share a host (the MVP persona), so dialing `127.0.0.1:<port>` reaches it. Only
/// the port is shared with the binary — the host differs per end — so both derive their address
/// from the one `DEFAULT_OSC_PORT` const and can never drift on it. Structure edits ride the
/// separate, loopback-only structure channel ([`DEFAULT_STRUCTURE_ADDR`]) — this is the OSC control
/// plane, not that.
pub fn default_osc_addr() -> String {
    format!("127.0.0.1:{DEFAULT_OSC_PORT}")
}

/// The engine-facing seam every engine tool drives. The shipping impl is
/// [`EngineLink`]; the unit tests inject an in-memory fake so the tool bodies are exercised
/// without a live `reuben play`. The three structure verbs mirror [`StructureClient`] one-for-one;
/// [`send_osc`](Self::send_osc) is the OSC/UDP control path; the endpoint getters
/// feed `engine_status`.
pub trait EngineChannel: Send + Sync {
    /// Structure-channel liveness: `Ok(())` iff the engine answered `pong`.
    fn ping(&self) -> Result<(), StructureError>;
    /// Install a document by path, with an optional `expect` content-hash guard.
    fn swap(
        &self,
        source: DocSource,
        expect: Option<String>,
    ) -> Result<SwapOutcome, StructureError>;
    /// Read the canonical installed document and its content hash.
    fn get_document(&self) -> Result<DocumentSnapshot, StructureError>;
    /// Read the running diagnostics counters.
    fn get_diagnostics(&self) -> Result<DiagnosticsReport, StructureError>;
    /// Dispatch a batch of already-encoded OSC datagrams to the engine's control endpoint,
    /// returning how many left the socket. UDP is fire-and-forget: a datagram out is "dispatched",
    /// not "received".
    fn send_osc(&self, datagrams: &[Vec<u8>]) -> std::io::Result<usize>;
    /// The structure-channel endpoint address, for `engine_status`.
    fn structure_endpoint(&self) -> String;
    /// The OSC control endpoint address, for `engine_status`.
    fn osc_endpoint(&self) -> String;
}

/// The shipping [`EngineChannel`]: the #315 [`StructureClient`] for the structure verbs plus the
/// engine's OSC-in address for the control path. Cheap to hold and clone; each structure exchange
/// opens its own short-lived connection (see [`StructureClient`]) and each `send` binds an
/// ephemeral UDP socket, so nothing is retained between calls and the link survives the engine
/// restarting under it.
#[derive(Debug, Clone)]
pub struct EngineLink {
    client: StructureClient,
    osc_addr: String,
}

impl EngineLink {
    /// A link dialing `structure_addr` for the structure channel and `osc_addr` for OSC control.
    pub fn new(structure_addr: impl Into<String>, osc_addr: impl Into<String>) -> Self {
        Self {
            client: StructureClient::new(structure_addr),
            osc_addr: osc_addr.into(),
        }
    }
}

impl Default for EngineLink {
    /// A link targeting the shared [`DEFAULT_STRUCTURE_ADDR`] and [`default_osc_addr`] — the same
    /// endpoints `reuben play` binds, so the sidecar and engine can never drift.
    fn default() -> Self {
        Self::new(DEFAULT_STRUCTURE_ADDR, default_osc_addr())
    }
}

impl EngineChannel for EngineLink {
    fn ping(&self) -> Result<(), StructureError> {
        self.client.ping()
    }

    fn swap(
        &self,
        source: DocSource,
        expect: Option<String>,
    ) -> Result<SwapOutcome, StructureError> {
        self.client.swap(source, expect)
    }

    fn get_document(&self) -> Result<DocumentSnapshot, StructureError> {
        self.client.get_document()
    }

    fn get_diagnostics(&self) -> Result<DiagnosticsReport, StructureError> {
        self.client.get_diagnostics()
    }

    fn send_osc(&self, datagrams: &[Vec<u8>]) -> std::io::Result<usize> {
        // Bind an ephemeral loopback socket per batch — `send` is an infrequent authoring gesture,
        // so a persistent socket buys nothing and a fresh one can't wedge. UDP `send_to` queues the
        // datagram; loopback delivery to a live engine does not fail at this layer.
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        let mut sent = 0;
        for datagram in datagrams {
            socket.send_to(datagram, &self.osc_addr)?;
            sent += 1;
        }
        Ok(sent)
    }

    fn structure_endpoint(&self) -> String {
        self.client.addr().to_string()
    }

    fn osc_endpoint(&self) -> String {
        self.osc_addr.clone()
    }
}

//! The two engine-facing planes the five engine tools drive, and the [`EngineLink`] that holds
//! both.
//!
//! The planes stay separate on purpose. Structure edits ride a **loopback-only** TCP/NDJSON channel
//! ([`StructureClient`], #315) because they are more powerful than OSC control; OSC control rides
//! UDP to an endpoint the engine binds on all interfaces. `EngineLink` owns one handle to each
//! rather than merging them, so a call site names the plane it is using.
//!
//! There is no engine-facing trait. The injectable seam is one layer down —
//! [`StructureTransport`](crate::StructureTransport), the socket itself — so tool-body tests
//! exercise real NDJSON serialization, real parsing, and the real unreachable classification
//! instead of a hand-written stand-in for them.
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

use reuben_core::coordinator::{DEFAULT_OSC_PORT, DEFAULT_STRUCTURE_ADDR};

use crate::client::StructureClient;

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

/// The two planes an engine tool can reach, held together: the [`StructureClient`] for the
/// structure channel (loopback TCP/NDJSON, #315) and the engine's OSC-in address for the control
/// path.
///
/// This is a **composition, not an abstraction** — there is no trait, and nothing here forwards.
/// `send` is the one tool that needs both planes (probe the structure channel, then dispatch
/// datagrams), which is the whole reason something has to own both handles. Call sites reach
/// [`structure()`](Self::structure) or [`send_osc`](Self::send_osc) and thereby name the plane they
/// mean; the injectable seam lives one layer down, at
/// [`StructureTransport`](crate::StructureTransport).
///
/// Cheap to hold: each structure exchange opens its own short-lived connection and each `send`
/// binds an ephemeral UDP socket, so nothing is retained between calls and the link survives the
/// engine restarting under it.
#[derive(Debug)]
pub struct EngineLink {
    structure: StructureClient,
    osc_addr: String,
}

impl EngineLink {
    /// A link dialing `structure_addr` for the structure channel and `osc_addr` for OSC control.
    pub fn new(structure_addr: impl Into<String>, osc_addr: impl Into<String>) -> Self {
        Self::from_parts(StructureClient::new(structure_addr), osc_addr)
    }

    /// A link over an already-built [`StructureClient`] — the injection point for tests, which
    /// pair a fake structure transport with an `osc_addr` pointing at a UDP socket they bound
    /// themselves.
    pub fn from_parts(structure: StructureClient, osc_addr: impl Into<String>) -> Self {
        Self {
            structure,
            osc_addr: osc_addr.into(),
        }
    }

    /// The structure channel — liveness, `swap`, `get_document`, `get_diagnostics`.
    pub fn structure(&self) -> &StructureClient {
        &self.structure
    }

    /// Dispatch a batch of already-encoded OSC datagrams to the engine's control endpoint,
    /// returning how many left the socket. UDP is fire-and-forget: a datagram out is "dispatched",
    /// not "received".
    pub fn send_osc(&self, datagrams: &[Vec<u8>]) -> std::io::Result<usize> {
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

    /// The structure-channel endpoint address, for `engine_status`.
    pub fn structure_endpoint(&self) -> String {
        self.structure.addr().to_string()
    }

    /// The OSC control endpoint address, for `engine_status`.
    pub fn osc_endpoint(&self) -> String {
        self.osc_addr.clone()
    }
}

impl Default for EngineLink {
    /// A link targeting the shared [`DEFAULT_STRUCTURE_ADDR`] and [`default_osc_addr`] — the same
    /// endpoints `reuben play` binds, so the sidecar and engine can never drift.
    fn default() -> Self {
        Self::new(DEFAULT_STRUCTURE_ADDR, default_osc_addr())
    }
}

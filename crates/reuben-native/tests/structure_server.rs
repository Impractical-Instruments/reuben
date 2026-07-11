//! Integration: the structure channel (ADR-0046 §8) end-to-end over a real loopback TCP
//! socket. Starts a [`StructureServer`] on the built-in default rig — everything `reuben play`
//! wires up except the cpal device (there is none in CI) — binds an **ephemeral** port
//! (`127.0.0.1:0`, OS-assigned, so parallel CI jobs never collide), then drives a plain
//! `TcpStream` client speaking NDJSON.
//!
//! Two behaviors under test:
//! - the three non-mutating verbs answer over the wire (pong; the default doc + its hash; a
//!   zeroed snapshot at startup), one framed response per request, in order;
//! - the server **shuts down cleanly** — every thread joined — even with an idle client still
//!   connected, the joinable stop that replaces `play`'s park-forever loop.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use reuben_core::coordinator::{DiagnosticsReport, DocSource, Request, Response};
use reuben_core::{content_hash, NormalizedDoc, Registry};
use reuben_native::diagnostics::Diagnostics;
use reuben_native::resources::FsResolver;
use reuben_native::rigs::DEFAULT_JSON;
use reuben_native::structure::{StructureServer, StructureState};

/// The canonical default-rig document and hash a fresh `play` would retain, plus a
/// [`StructureState`] over the given diagnostics. Built exactly as `play` builds it — the same
/// normalization (`Engine::from_document` mints the same doc), so the expected value here is
/// what a real engine would report.
fn default_rig_state(diagnostics: Arc<Diagnostics>) -> (StructureState, serde_json::Value, String) {
    let resolver = FsResolver::new(".");
    let doc = NormalizedDoc::from_json(
        DEFAULT_JSON,
        &Registry::builtin(),
        Some(&resolver as &dyn reuben_core::resources::ResourceResolver),
    )
    .expect("the default rig normalizes");
    let expected_doc = serde_json::to_value(&*doc).expect("canonical doc serializes");
    let expected_hash = content_hash(&doc);
    (
        StructureState::from_doc(&doc, diagnostics),
        expected_doc,
        expected_hash,
    )
}

/// Read one newline-framed [`Response`] off the wire, asserting the framing (ADR-0046 §8: one
/// JSON object per line, newline-terminated).
fn read_response(reader: &mut impl BufRead) -> Response {
    let mut line = String::new();
    let n = reader.read_line(&mut line).expect("read a response line");
    assert!(n > 0, "server closed the connection without responding");
    assert!(
        line.ends_with('\n'),
        "a response is one newline-terminated line: {line:?}"
    );
    assert_eq!(
        line.matches('\n').count(),
        1,
        "exactly one response per line: {line:?}"
    );
    Response::from_ndjson(&line).expect("a response parses as JSON")
}

/// Run `f` on a helper thread and fail if it does not finish within `secs` — a hang assertion,
/// so a shutdown that never joins fails the test loudly instead of stalling the suite.
fn within<F: FnOnce() + Send + 'static>(secs: u64, f: F) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    assert!(
        rx.recv_timeout(Duration::from_secs(secs)).is_ok(),
        "operation did not complete within {secs}s — it hung"
    );
}

#[test]
fn serves_the_three_verbs_over_loopback_ndjson_in_order() {
    let diagnostics = Diagnostics::new();
    let (state, expected_doc, expected_hash) = default_rig_state(Arc::clone(&diagnostics));
    let server = StructureServer::bind("127.0.0.1:0", state).expect("bind loopback structure port");
    let addr = server.local_addr();
    assert!(
        addr.ip().is_loopback(),
        "structure channel is loopback-only"
    );

    let client = TcpStream::connect(addr).expect("connect to structure channel");
    let mut writer = client.try_clone().expect("clone client for writing");
    let mut reader = BufReader::new(client);

    // ping -> Pong (the channel proves itself alive, ADR-0044 §2).
    writer
        .write_all(Request::Ping.to_ndjson().as_bytes())
        .unwrap();
    assert_eq!(read_response(&mut reader), Response::Pong);

    // get_document -> the retained canonical default rig + its content hash (ADR-0046 §7/§9).
    writer
        .write_all(Request::GetDocument.to_ndjson().as_bytes())
        .unwrap();
    match read_response(&mut reader) {
        Response::Document {
            document,
            content_hash: hash,
        } => {
            assert_eq!(
                document, expected_doc,
                "the served doc is the canonical default rig"
            );
            assert_eq!(hash, expected_hash, "and carries its content hash");
            assert_eq!(document["instrument"], serde_json::json!("default"));
            assert_eq!(
                document["format_version"],
                serde_json::json!(3),
                "the document is normalized to the current format version"
            );
        }
        other => panic!("expected Document, got {other:?}"),
    }

    // get_diagnostics -> a zeroed snapshot at startup (ADR-0038 §9 / ADR-0048 §6).
    writer
        .write_all(Request::GetDiagnostics.to_ndjson().as_bytes())
        .unwrap();
    assert_eq!(
        read_response(&mut reader),
        Response::Diagnostics(DiagnosticsReport::default())
    );

    server.shutdown();
}

#[test]
fn responses_come_back_one_per_request_in_pipelined_order() {
    // ADR-0046 §8: one response per request, in order. Pipeline three requests before reading
    // any reply, then assert the replies arrive matched to the request order.
    let (state, _, _) = default_rig_state(Diagnostics::new());
    let server = StructureServer::bind("127.0.0.1:0", state).expect("bind");
    let client = TcpStream::connect(server.local_addr()).expect("connect");
    let mut writer = client.try_clone().unwrap();
    let mut reader = BufReader::new(client);

    for req in [Request::GetDiagnostics, Request::Ping, Request::GetDocument] {
        writer.write_all(req.to_ndjson().as_bytes()).unwrap();
    }
    assert!(matches!(
        read_response(&mut reader),
        Response::Diagnostics(_)
    ));
    assert_eq!(read_response(&mut reader), Response::Pong);
    assert!(matches!(
        read_response(&mut reader),
        Response::Document { .. }
    ));

    server.shutdown();
}

#[test]
fn get_diagnostics_reflects_live_counter_bumps() {
    // The endpoint reads the live Arc audio::start owns, not a copy frozen at startup: a
    // counter bumped after the server started is visible on the next query.
    let diagnostics = Diagnostics::new();
    let (state, _, _) = default_rig_state(Arc::clone(&diagnostics));
    let server = StructureServer::bind("127.0.0.1:0", state).expect("bind");
    let client = TcpStream::connect(server.local_addr()).expect("connect");
    let mut writer = client.try_clone().unwrap();
    let mut reader = BufReader::new(client);

    diagnostics.record_output_xrun();
    diagnostics.record_input_ring_underrun_frames(7);
    writer
        .write_all(Request::GetDiagnostics.to_ndjson().as_bytes())
        .unwrap();
    assert_eq!(
        read_response(&mut reader),
        Response::Diagnostics(DiagnosticsReport {
            output_xruns: 1,
            input_ring_underruns: 7,
            ..DiagnosticsReport::default()
        })
    );

    server.shutdown();
}

#[test]
fn swap_is_answered_but_not_installed() {
    let (state, _, _) = default_rig_state(Diagnostics::new());
    let server = StructureServer::bind("127.0.0.1:0", state).expect("bind");
    let client = TcpStream::connect(server.local_addr()).expect("connect");
    let mut writer = client.try_clone().unwrap();
    let mut reader = BufReader::new(client);

    let req = Request::Swap {
        source: DocSource::Path("instruments/warm-pad.json".to_string()),
        expect: None,
    };
    writer.write_all(req.to_ndjson().as_bytes()).unwrap();
    match read_response(&mut reader) {
        Response::Error { message } => assert!(
            message.contains("not yet implemented"),
            "swap is a not-yet-implemented Error in M1: {message:?}"
        ),
        other => panic!("swap must return Error in M1, got {other:?}"),
    }

    server.shutdown();
}

#[test]
fn shuts_down_cleanly_with_an_idle_client_still_connected() {
    // The behavioral shutdown proof: a handler blocked on `read_line` for an idle client must
    // not keep the server alive. `shutdown()` joins every thread; if it can't, `within` fails
    // the test rather than hanging the suite — the park-forever hang this ticket removes.
    let diagnostics = Diagnostics::new();
    let (state, _, _) = default_rig_state(Arc::clone(&diagnostics));
    let server = StructureServer::bind("127.0.0.1:0", state).expect("bind");
    // Connect and leave it idle: the handler thread is now parked in a blocking read.
    let _idle = TcpStream::connect(server.local_addr()).expect("connect");

    within(10, move || {
        server.shutdown();
        // The final exit-time snapshot flush `play` performs (ADR-0038 §9) — part of the same
        // teardown, exercised here so the whole clean-shutdown sequence is covered off-audio.
        reuben_native::diagnostics::log_snapshot(&diagnostics.snapshot());
    });
}

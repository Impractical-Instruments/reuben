//! This door against a real engine: start one through reuben-native's entry point — everything
//! `reuben play` starts, with the audio device stood in for by the shared fake callback, since CI
//! has none — then reach it the way the sidecar does, over this crate's loopback TCP transport, and
//! the way an embedding binary does, over the entry point's in-process channel. Both must land on
//! the one Coordinator, and shutting the engine down must free every port it bound.

use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use reuben_api::engine::{
    get_current_instrument, get_engine_status, send_live_controls, swap_instrument,
    SendLiveControls, SwapInstrument, IN_PROCESS_ENDPOINT,
};
use reuben_api::FsResolver;
use reuben_mcp::EngineLink;
use reuben_native::engine::{EngineConfig, Instrument};
use reuben_native::test_support::start_headless;

const OSC_DOC: &str = r#"{"format_version":3,"instrument":"t",
    "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
    "nodes":[{"type":"oscillator","address":"/osc"}]}"#;

const ENVELOPE_DOC: &str = r#"{ "format_version": 3, "instrument": "eg",
    "interface": { "outputs": { "out": { "from": "/out.audio" } } },
    "nodes": [
      { "type": "envelope", "address": "/env",
        "inputs": { "gate": 1.0, "attack": 0.5, "decay": 0.01, "sustain": 0.8, "release": 0.5 } },
      { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/env.cv" } } } ] }"#;

/// A fresh directory holding `docs`, unique to this test and process.
fn seed(test: &str, docs: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "reuben_native_engine_{test}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the test directory");
    for (name, body) in docs {
        std::fs::write(dir.join(name), body).expect("seed a document");
    }
    dir
}

/// The entry point's config for a test: ephemeral loopback ports throughout, so parallel runs and a
/// `reuben play` on the same machine never collide.
fn config(instrument: PathBuf) -> EngineConfig {
    EngineConfig {
        osc_in: Some("127.0.0.1:0".to_string()),
        structure: Some("127.0.0.1:0".to_string()),
        ..EngineConfig::new(Instrument::Path(instrument))
    }
}

/// Run `f` on a helper thread and fail if it does not finish within `secs` — a hang assertion.
fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(secs))
        .unwrap_or_else(|_| panic!("did not complete within {secs}s — it hung"))
}

#[test]
fn the_sidecar_reaches_an_engine_started_through_the_entry_point() {
    let dir = seed("status", &[("osc.json", OSC_DOC)]);
    let engine = start_headless(config(dir.join("osc.json"))).expect("the engine starts");
    let structure = engine.structure_addr().expect("the structure server bound");

    // Exactly what the sidecar's `get_engine_status` tool runs, over the link it builds.
    let link = EngineLink::new(structure.to_string());
    let status = get_engine_status(link.structure(), env!("CARGO_PKG_VERSION"));
    assert!(status.output.reachable, "{}", status.summary);
    assert_eq!(status.output.endpoints.structure, structure.to_string());

    let in_process = get_engine_status(&engine.channel(), env!("CARGO_PKG_VERSION"));
    assert!(in_process.output.reachable, "{}", in_process.summary);
    assert_eq!(in_process.output.endpoints.structure, IN_PROCESS_ENDPOINT);

    engine.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_in_process_swap_is_what_the_sidecar_reads_next() {
    let dir = seed("swap", &[("osc.json", OSC_DOC), ("eg.json", ENVELOPE_DOC)]);
    let engine = start_headless(config(dir.join("osc.json"))).expect("the engine starts");
    let link = EngineLink::new(engine.structure_addr().expect("bound").to_string());
    let store = |_: Option<&str>| FsResolver::new(&dir);

    let before = get_current_instrument(link.structure(), store).expect("read over TCP");
    assert_eq!(
        before.output.source.as_deref(),
        Some(dir.join("osc.json").display().to_string().as_str())
    );

    let target = dir.join("eg.json").display().to_string();
    let swapped = swap_instrument(
        &SwapInstrument {
            path: target.clone(),
            expect: Some(before.output.content_hash.clone()),
        },
        &engine.channel(),
    )
    .expect("the in-process swap reached the engine");
    assert!(swapped.output.report.report.ok, "{}", swapped.summary);
    assert!(swapped.output.conflict.is_none());

    let after = get_current_instrument(link.structure(), store).expect("read over TCP");
    assert_eq!(after.output.source.as_deref(), Some(target.as_str()));
    assert_eq!(
        after.output.content_hash,
        swapped.output.report.content_hash
    );
    assert_ne!(after.output.content_hash, before.output.content_hash);

    let gesture: SendLiveControls = serde_json::from_value(serde_json::json!({
        "messages": [{ "address": "/env/gate", "args": [0.0] }]
    }))
    .expect("a control batch");
    let sent = send_live_controls(&gesture, &engine.channel()).expect("queued in-process");
    assert_eq!(sent.output.sent, 1);

    engine.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shutdown_stops_every_thread_and_frees_every_port() {
    let dir = seed("shutdown", &[("osc.json", OSC_DOC)]);
    let instrument = dir.join("osc.json");
    // The engine is not `Send` (cpal's streams are not), so it lives and dies on the watched thread.
    // No client connects before the rebind below: an accepted connection the server closed would
    // leave the port in TIME_WAIT, which is not what this test is about.
    let (structure, osc_in) = within(10, move || {
        let engine = start_headless(config(instrument)).expect("the engine starts");
        let structure: SocketAddr = engine.structure_addr().expect("bound");
        let osc_in: SocketAddr = engine.osc_in_addr().expect("bound");
        engine.shutdown();
        (structure, osc_in)
    });

    TcpListener::bind(structure).expect("the structure port is free again");
    UdpSocket::bind(osc_in).expect("the OSC-in port is free again");
    let _ = std::fs::remove_dir_all(&dir);
}

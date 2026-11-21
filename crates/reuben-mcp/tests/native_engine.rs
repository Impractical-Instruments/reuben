//! This door against a real engine: start one through reuben-native's entry point — everything
//! `reuben play` starts, with the audio device stood in for by the shared fake callback, since CI
//! has none — then reach it the way the sidecar does, over this crate's loopback TCP transport, and
//! the way an embedding binary does, over the entry point's in-process channel. Both must land on
//! the one Coordinator, and shutting the engine down must free every port it bound. The sidecar
//! binary itself, started with the engine's address, must edit what plays.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use reuben_api::engine::{
    get_engine_status, open_instrument, send_live_controls, OpenInstrument, SendLiveControls,
    IN_PROCESS_ENDPOINT,
};
use reuben_api::tools::names;
use reuben_mcp::{EngineLink, STRUCTURE_ADDR_ENV};
use reuben_native::engine::{EngineConfig, Instrument};
use reuben_native::test_support::{start_headless, timed_teardown, within};

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

/// The hang deadline, generous next to the shutdown bound so the two fail differently.
const HANG_SECS: u64 = 10;

/// The hang deadline for a test that also starts the sidecar binary.
const SIDECAR_HANG_SECS: u64 = 30;

fn gate_off() -> SendLiveControls {
    serde_json::from_value(serde_json::json!({
        "messages": [{ "address": "/env/gate", "args": [0.0] }]
    }))
    .expect("a control batch")
}

// A `RunningEngine` is not `Send`, so every test below starts, drives and tears one down entirely
// inside `within`.

#[test]
fn the_sidecar_reaches_an_engine_started_through_the_entry_point() {
    let dir = seed("status", &[("osc.json", OSC_DOC)]);
    let instrument = dir.join("osc.json");
    within(HANG_SECS, move || {
        let engine = start_headless(config(instrument)).expect("the engine starts");
        let structure = engine.structure_addr().expect("the structure server bound");

        // Exactly what the sidecar's `get_engine_status` tool runs, over the link it builds.
        let link = EngineLink::new(structure.to_string(), None);
        let status = get_engine_status(link.structure(), env!("CARGO_PKG_VERSION"));
        assert!(status.output.reachable, "{}", status.summary);
        assert_eq!(status.output.endpoints.structure, structure.to_string());

        let in_process = get_engine_status(&engine.channel(), env!("CARGO_PKG_VERSION"));
        assert!(in_process.output.reachable, "{}", in_process.summary);
        assert_eq!(in_process.output.endpoints.structure, IN_PROCESS_ENDPOINT);

        timed_teardown(engine, false);
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// The sidecar binary as an agent's MCP client runs it, dialing the engine at `structure` through
/// [`STRUCTURE_ADDR_ENV`], one request at a time.
struct Sidecar {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Sidecar {
    fn start(structure: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_reuben-mcp"))
            .env(STRUCTURE_ADDR_ENV, structure)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the reuben-mcp binary");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        let mut sidecar = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        sidecar.request(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "reuben-mcp-it", "version": "0.0.0" },
            }),
        );
        writeln!(
            sidecar.stdin,
            r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
        )
        .expect("write initialized");
        sidecar
    }

    /// Send one request and return its response's `result`.
    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        let line =
            serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{line}").expect("write the request");
        self.stdin.flush().expect("flush the request");
        loop {
            let mut reply = String::new();
            let read = self.stdout.read_line(&mut reply).expect("read a reply");
            assert_ne!(
                read, 0,
                "the sidecar closed stdout before answering {method}"
            );
            let Ok(reply) = serde_json::from_str::<serde_json::Value>(&reply) else {
                continue;
            };
            if reply["id"] == serde_json::json!(id) {
                return reply
                    .get("result")
                    .cloned()
                    .unwrap_or_else(|| panic!("{method} carried no result: {reply}"));
            }
        }
    }

    /// One `tools/call`, as its `CallToolResult`.
    fn call(&mut self, tool: &str, arguments: serde_json::Value) -> serde_json::Value {
        self.request(
            "tools/call",
            serde_json::json!({ "name": tool, "arguments": arguments }),
        )
    }

    /// One `tools/call` that must not be `isError`, as its structured payload.
    fn structured(&mut self, tool: &str, arguments: serde_json::Value) -> serde_json::Value {
        let result = self.call(tool, arguments);
        assert_ne!(
            result["isError"],
            serde_json::json!(true),
            "{tool}: {result}"
        );
        result["structuredContent"].clone()
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The names `list_instruments` offers.
fn listed(sidecar: &mut Sidecar) -> Vec<String> {
    let listed = sidecar.structured(
        names::LIST_INSTRUMENTS,
        serde_json::json!({ "what": "instruments" }),
    );
    serde_json::from_value(listed["names"].clone())
        .unwrap_or_else(|e| panic!("a list of names ({e}): {listed}"))
}

/// An agent's edit, through the sidecar to a real engine, is what plays — no swap — and an edit to
/// a document the engine is not playing is refused until the agent opens it. The engine owns the
/// files under its instrument root and the sidecar runs from somewhere else, so a name resolved
/// against the sidecar's working directory would miss.
#[test]
fn an_agent_edit_is_live() {
    let dir = seed("live", &[("osc.json", OSC_DOC)]);
    let root = dir.clone();
    within(SIDECAR_HANG_SECS, move || {
        assert_ne!(
            std::env::current_dir().expect("cwd").canonicalize().ok(),
            root.canonicalize().ok(),
            "the sidecar must not run from the instrument root"
        );
        let engine = start_headless(EngineConfig {
            instrument_root: Some(root.clone()),
            ..config(root.join("osc.json"))
        })
        .expect("the engine starts");
        let mut sidecar = Sidecar::start(&engine.structure_addr().expect("bound").to_string());

        let names_offered = listed(&mut sidecar);
        assert_eq!(names_offered.len(), 1, "{names_offered:?}");
        let seeded = names_offered[0].clone();

        let edit = sidecar.structured(
            names::SET_INSTRUMENT_INPUT,
            serde_json::json!({
                "source": seeded, "address": "/osc", "input": "freq", "value": 330.0
            }),
        );
        assert_eq!(edit["written"], serde_json::json!(true), "{edit}");
        let playing = sidecar.structured(names::GET_CURRENT_INSTRUMENT, serde_json::json!({}));
        assert_eq!(
            playing["content_hash"], edit["hash"],
            "the accepted edit is what plays, with no swap: {playing}"
        );

        std::fs::write(root.join("eg.json"), ENVELOPE_DOC).expect("write a second document");
        let other = listed(&mut sidecar)
            .into_iter()
            .find(|name| *name != seeded)
            .expect("the second document is listed");
        let rename = serde_json::json!({ "source": other, "name": "renamed" });

        let refused = sidecar.call(names::SET_INSTRUMENT_NAME, rename.clone());
        assert_eq!(
            refused["structuredContent"]["written"],
            serde_json::json!(false),
            "an edit to a document that is not open is not written: {refused}"
        );
        assert!(
            refused.to_string().contains("open it to edit it"),
            "the refusal says to open it: {refused}"
        );

        let opened =
            sidecar.structured(names::OPEN_INSTRUMENT, serde_json::json!({ "name": other }));
        assert_eq!(opened["ok"], serde_json::json!(true), "{opened}");
        let edit = sidecar.structured(names::SET_INSTRUMENT_NAME, rename);
        assert_eq!(edit["written"], serde_json::json!(true), "{edit}");

        drop(sidecar);
        timed_teardown(engine, true);
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// A channel the embedder kept past shutdown answers the way a dead TCP engine does, on every
/// verb — never with a success from a Coordinator whose render side is gone.
#[test]
fn a_channel_held_past_shutdown_is_unreachable() {
    let dir = seed("held", &[("osc.json", OSC_DOC), ("eg.json", ENVELOPE_DOC)]);
    let instrument = dir.join("osc.json");
    let target = dir.join("eg.json").display().to_string();
    let channel = within(HANG_SECS, move || {
        let engine = start_headless(config(instrument)).expect("the engine starts");
        let channel = engine.channel();
        timed_teardown(engine, false);
        channel
    });

    let status = get_engine_status(&channel, env!("CARGO_PKG_VERSION"));
    assert!(!status.output.reachable, "{}", status.summary);
    let open = open_instrument(
        &OpenInstrument {
            name: target,
            expect: None,
        },
        &channel,
    );
    assert!(
        open.is_err(),
        "an open after shutdown must not report success"
    );
    assert!(send_live_controls(&gate_off(), &channel).is_err());
    assert!(channel
        .get_document()
        .expect_err("shut down")
        .is_unreachable());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shutdown_stops_every_thread_and_frees_every_port() {
    let dir = seed("shutdown", &[("osc.json", OSC_DOC)]);
    let instrument = dir.join("osc.json");
    // No client connects before the rebind below: an accepted connection the server closed would
    // leave the port in TIME_WAIT, which is not what this test is about.
    let (structure, osc_in) = within(HANG_SECS, move || {
        let engine = start_headless(config(instrument)).expect("the engine starts");
        let structure: SocketAddr = engine.structure_addr().expect("bound");
        let osc_in: SocketAddr = engine.osc_in_addr().expect("bound");
        timed_teardown(engine, false);
        (structure, osc_in)
    });
    TcpListener::bind(structure).expect("the structure port is free again");
    UdpSocket::bind(osc_in).expect("the OSC-in port is free again");
    let _ = std::fs::remove_dir_all(&dir);
}

//! Integration: the M2 structure channel (ADR-0046 §8) end-to-end over a real loopback TCP
//! socket. Starts a [`StructureServer`] wired to a real [`Coordinator`] — everything `reuben play`
//! wires up except the cpal device (there is none in CI) — binds an **ephemeral** port
//! (`127.0.0.1:0`, OS-assigned, so parallel CI jobs never collide), then drives a plain
//! `TcpStream` client speaking NDJSON.
//!
//! The device half is stood in for by a **fake audio callback** (ADR-0053 §4): a background thread
//! owning the [`RenderSlot`] the real cpal callback would, driving it in a loop so a swap installs
//! **via the install mailbox** (not a stream restart) and the Coordinator's off-thread `reclaim`
//! completes — all with no audio device. The audible/device-gap half stays a scripted human test
//! (`docs/rituals/m2-swap-ramp-duck.md`).
//!
//! Behaviors under test:
//! - the three non-mutating verbs answer over the wire, one framed response per request, in order;
//! - the **`swap`** verb (ADR-0046 §§1–10) installs a new document over the wire **via the
//!   mailbox** — `get_document` then reports the new doc + hash, and the [`SwapReport`] carries
//!   **real** survivor stats (`survived: 2` for an identical-document swap — impossible under M1's
//!   all-cold restart, which hard-codes `survived: 0`);
//! - an **input-binding swap onto an output-only stream** dark-degrades to silence with a loud
//!   warning and stays alive (ADR-0038 §7/§9) — not an error, not a crash;
//! - `expect` arbitration (ADR-0046 §9) conflicts on a stale hash and proceeds on a matching one;
//!   a bad document reports errors with no install;
//! - the server **shuts down cleanly** — every thread joined — even with an idle client connected.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use reuben_core::coordinator::{Coordinator, DiagnosticsReport, DocSource, Request, Response};
use reuben_core::coordinator::{RenderSide, RenderSlot};
use reuben_core::resources::MemoryResolver;
use reuben_core::{content_hash, AudioConfig, NormalizedDoc, Registry};
use reuben_native::diagnostics::Diagnostics;
use reuben_native::structure::{HeadlessRenderConfig, StructureServer, StructureState};

fn cfg() -> AudioConfig {
    AudioConfig::new(48_000.0, 128)
}

/// A minimal output-only rig: one oscillator through a master output. Self-contained (no
/// resources), so a bare [`MemoryResolver`] loads it.
const BASE_DOC: &str = r#"{"format_version":3,"instrument":"t",
    "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
    "nodes":[{"type":"oscillator","address":"/osc"}]}"#;

/// A held envelope whose CV is the master output. Two nodes (`/env`, `/out`); a swap to the
/// identical document keeps both survivors, so the real migration diff carries `survived: 2`.
fn envelope_doc(env_addr: &str) -> String {
    format!(
        r#"{{ "format_version": 3, "instrument": "eg",
             "interface": {{ "outputs": {{ "out": {{ "from": "/out.audio" }} }} }},
             "nodes": [
               {{ "type": "envelope", "address": "{env_addr}",
                  "inputs": {{ "gate": 1.0, "attack": 0.5, "decay": 0.01,
                               "sustain": 0.8, "release": 0.5 }} }},
               {{ "type": "output", "address": "/out",
                  "inputs": {{ "audio": {{ "from": "{env_addr}.cv" }} }} }} ] }}"#
    )
}

/// A minimal instrument binding logical input channel 0 (ADR-0038 §3) — live input an output-only
/// stream can't provide, the dark-degrade case.
const MIC_PASSTHRU: &str = r#"{ "format_version": 3, "instrument": "mic-passthru",
    "interface": {
        "inputs": { "mic": { "type": "f32_buffer", "channel": 0 } },
        "outputs": { "out": { "from": "/mic" } } },
    "nodes": [] }"#;

/// A background "fake audio callback": owns the [`RenderSlot`] the real cpal callback would and
/// drives it in a loop, draining the install mailbox, running the master-gain ramp, transplanting
/// survivors, and posting retirees — so a Coordinator-driven swap installs via the mailbox and its
/// `reclaim` completes, with no audio device.
struct FakeCallback {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl FakeCallback {
    fn spawn(side: RenderSide) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let mut slot = RenderSlot::new(side);
            let mut buf = vec![0.0f32; 128 * slot.channels().max(1)];
            while !stop_thread.load(Ordering::SeqCst) {
                let ch = slot.channels().max(1);
                if buf.len() != 128 * ch {
                    buf.resize(128 * ch, 0.0);
                }
                slot.fill(&mut buf);
                std::thread::sleep(Duration::from_millis(1));
            }
            // The slot (Engine + mailbox) drops here, off any RT thread.
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// A Coordinator-backed [`StructureState`] over `doc`, a live [`FakeCallback`] draining its mailbox,
/// and the base document's content hash. `opened_input_channels` is the headless render config's
/// input-stream geometry (`0` = output-only, so an input-binding swap dark-degrades).
fn wired(doc: &str, opened_input_channels: usize) -> (StructureState, FakeCallback, String) {
    let (coordinator, side, _warnings) = Coordinator::install_initial(
        doc,
        Registry::builtin(),
        Box::new(MemoryResolver::new()),
        cfg(),
    )
    .expect("initial install");
    let base_hash = coordinator.installed_hash();
    let state = StructureState::from_coordinator(coordinator, Diagnostics::new())
        .with_render_config(Arc::new(HeadlessRenderConfig {
            opened_input_channels,
        }));
    (state, FakeCallback::spawn(side), base_hash)
}

/// Read one newline-framed [`Response`] off the wire, asserting the framing (ADR-0046 §8).
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

fn send(writer: &mut impl Write, req: &Request) {
    writer
        .write_all(req.to_ndjson().as_bytes())
        .expect("send request");
}

/// Run `f` on a helper thread and fail if it does not finish within `secs` — a hang assertion.
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

fn expected_hash(doc: &str) -> String {
    content_hash(&NormalizedDoc::from_json(doc, &Registry::builtin(), None).expect("mint"))
}

#[test]
fn serves_the_three_verbs_over_loopback_ndjson_in_order() {
    let (state, cb, base_hash) = wired(BASE_DOC, 0);
    let server = StructureServer::bind("127.0.0.1:0", state).expect("bind loopback structure port");
    let addr = server.local_addr();
    assert!(
        addr.ip().is_loopback(),
        "structure channel is loopback-only"
    );

    let client = TcpStream::connect(addr).expect("connect to structure channel");
    let mut writer = client.try_clone().expect("clone client for writing");
    let mut reader = BufReader::new(client);

    send(&mut writer, &Request::Ping);
    assert_eq!(read_response(&mut reader), Response::Pong);

    send(&mut writer, &Request::GetDocument);
    match read_response(&mut reader) {
        Response::Document {
            document,
            content_hash: hash,
        } => {
            assert_eq!(document["instrument"], serde_json::json!("t"));
            assert_eq!(hash, base_hash, "the served doc carries its content hash");
            assert_eq!(
                document["format_version"],
                serde_json::json!(3),
                "the document is normalized to the current format version"
            );
        }
        other => panic!("expected Document, got {other:?}"),
    }

    send(&mut writer, &Request::GetDiagnostics);
    assert_eq!(
        read_response(&mut reader),
        Response::Diagnostics(DiagnosticsReport::default())
    );

    server.shutdown();
    cb.stop();
}

#[test]
fn responses_come_back_one_per_request_in_pipelined_order() {
    let (state, cb, _) = wired(BASE_DOC, 0);
    let server = StructureServer::bind("127.0.0.1:0", state).expect("bind");
    let client = TcpStream::connect(server.local_addr()).expect("connect");
    let mut writer = client.try_clone().unwrap();
    let mut reader = BufReader::new(client);

    for req in [Request::GetDiagnostics, Request::Ping, Request::GetDocument] {
        send(&mut writer, &req);
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
    cb.stop();
}

#[test]
fn get_diagnostics_reflects_live_counter_bumps() {
    // The endpoint reads the live Arc audio::start owns, not a copy frozen at startup.
    let (coordinator, side, _w) = Coordinator::install_initial(
        BASE_DOC,
        Registry::builtin(),
        Box::new(MemoryResolver::new()),
        cfg(),
    )
    .expect("install");
    let diagnostics = Diagnostics::new();
    let state = StructureState::from_coordinator(coordinator, Arc::clone(&diagnostics));
    let cb = FakeCallback::spawn(side);
    let server = StructureServer::bind("127.0.0.1:0", state).expect("bind");
    let client = TcpStream::connect(server.local_addr()).expect("connect");
    let mut writer = client.try_clone().unwrap();
    let mut reader = BufReader::new(client);

    diagnostics.record_output_xrun();
    diagnostics.record_input_ring_underrun_frames(7);
    send(&mut writer, &Request::GetDiagnostics);
    assert_eq!(
        read_response(&mut reader),
        Response::Diagnostics(DiagnosticsReport {
            output_xruns: 1,
            input_ring_underruns: 7,
            ..DiagnosticsReport::default()
        })
    );

    server.shutdown();
    cb.stop();
}

#[test]
fn swap_over_the_wire_installs_via_the_mailbox_with_real_survivor_stats() {
    // The heart of M2: a swap to the identical envelope document installs over the wire through the
    // mailbox — the FakeCallback drains it (its `reclaim` completing is the proof, and no
    // SwapInstaller/stream restart exists to invoke) — and the real diff carries `survived: 2`
    // (both nodes), impossible under M1's all-cold restart. `get_document` then reports the new doc.
    let base = envelope_doc("/env");
    let (state, cb, base_hash) = wired(&base, 0);
    let server = StructureServer::bind("127.0.0.1:0", state).expect("bind");
    let client = TcpStream::connect(server.local_addr()).expect("connect");
    let mut writer = client.try_clone().unwrap();
    let mut reader = BufReader::new(client);

    let target: serde_json::Value = serde_json::from_str(&base).unwrap();
    send(
        &mut writer,
        &Request::Swap {
            source: DocSource::Document(target),
            expect: None,
        },
    );
    match read_response(&mut reader) {
        Response::SwapReport(report) => {
            assert!(report.report.ok, "a valid document installs: {report:?}");
            let diff = report.diff.expect("a successful swap carries a diff");
            assert_eq!(
                diff.survived, 2,
                "both nodes survive an identical-document swap (real migration): {diff:?}"
            );
            assert_eq!(report.content_hash, base_hash, "same doc -> same hash");
        }
        other => panic!("expected SwapReport, got {other:?}"),
    }

    // A second, renamed swap: only `/out` survives — proves back-to-back swaps turn over through
    // the mailbox (the first's retiree was reclaimed) with real, changing survivor stats.
    send(
        &mut writer,
        &Request::Swap {
            source: DocSource::Document(serde_json::from_str(&envelope_doc("/eg")).unwrap()),
            expect: None,
        },
    );
    match read_response(&mut reader) {
        Response::SwapReport(report) => {
            assert!(report.report.ok, "{report:?}");
            let diff = report.diff.expect("diff");
            assert_eq!(diff.survived, 1, "only /out survives a rename: {diff:?}");
        }
        other => panic!("expected SwapReport, got {other:?}"),
    }

    // get_document reflects the last installed document.
    send(&mut writer, &Request::GetDocument);
    match read_response(&mut reader) {
        Response::Document {
            document,
            content_hash: hash,
        } => {
            assert_eq!(document["instrument"], serde_json::json!("eg"));
            assert_eq!(hash, expected_hash(&envelope_doc("/eg")));
            assert_ne!(hash, base_hash, "the swap changed the installed document");
        }
        other => panic!("expected Document, got {other:?}"),
    }

    server.shutdown();
    cb.stop();
}

#[test]
fn swap_by_path_over_the_wire_installs() {
    let base = envelope_doc("/env");
    let (state, cb, _) = wired(&base, 0);
    let server = StructureServer::bind("127.0.0.1:0", state).expect("bind");
    let client = TcpStream::connect(server.local_addr()).expect("connect");
    let mut writer = client.try_clone().unwrap();
    let mut reader = BufReader::new(client);

    let path = std::env::temp_dir().join(format!("reuben_swap_wire_{}.json", std::process::id()));
    std::fs::write(&path, envelope_doc("/eg")).expect("write swap target");
    send(
        &mut writer,
        &Request::Swap {
            source: DocSource::Path(path.display().to_string()),
            expect: None,
        },
    );
    match read_response(&mut reader) {
        Response::SwapReport(report) => {
            assert!(report.report.ok, "a valid file installs: {report:?}")
        }
        other => panic!("expected SwapReport, got {other:?}"),
    }
    send(&mut writer, &Request::GetDocument);
    match read_response(&mut reader) {
        Response::Document { document, .. } => {
            assert_eq!(document["instrument"], serde_json::json!("eg"))
        }
        other => panic!("expected Document, got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);

    server.shutdown();
    cb.stop();
}

#[test]
fn input_binding_swap_over_the_wire_dark_degrades_and_stays_alive() {
    // ADR-0038 §7/§9: the initial rig is output-only (no input stream). A swap to an instrument
    // that binds an input channel installs and stays silent-but-alive — a loud swap-report WARNING,
    // never an error or a crash. The FakeCallback keeps rendering (the process stays alive).
    let (state, cb, _) = wired(BASE_DOC, 0);
    let server = StructureServer::bind("127.0.0.1:0", state).expect("bind");
    let client = TcpStream::connect(server.local_addr()).expect("connect");
    let mut writer = client.try_clone().unwrap();
    let mut reader = BufReader::new(client);

    send(
        &mut writer,
        &Request::Swap {
            source: DocSource::Document(serde_json::from_str(MIC_PASSTHRU).unwrap()),
            expect: None,
        },
    );
    match read_response(&mut reader) {
        Response::SwapReport(report) => {
            assert!(
                report.report.ok,
                "dark-degrade is not an error — the engine stays alive: {report:?}"
            );
            assert!(
                report
                    .report
                    .warnings
                    .iter()
                    .any(|w| w.message.contains("dark-degrade")),
                "the swap report carries the loud dark-degrade warning: {report:?}"
            );
        }
        other => panic!("expected SwapReport, got {other:?}"),
    }

    // Still alive afterward: the channel answers, and the mic doc is what's installed.
    send(&mut writer, &Request::GetDocument);
    match read_response(&mut reader) {
        Response::Document { document, .. } => {
            assert_eq!(document["instrument"], serde_json::json!("mic-passthru"))
        }
        other => panic!("expected Document, got {other:?}"),
    }

    server.shutdown();
    cb.stop();
}

#[test]
fn swap_of_a_bad_document_over_the_wire_reports_errors_without_installing() {
    let (state, cb, base_hash) = wired(BASE_DOC, 0);
    let server = StructureServer::bind("127.0.0.1:0", state).expect("bind");
    let client = TcpStream::connect(server.local_addr()).expect("connect");
    let mut writer = client.try_clone().unwrap();
    let mut reader = BufReader::new(client);

    let bad: serde_json::Value = serde_json::json!({
        "format_version": 3,
        "instrument": "bad",
        "nodes": [ { "type": "no_such_operator", "address": "/x" } ]
    });
    send(
        &mut writer,
        &Request::Swap {
            source: DocSource::Document(bad),
            expect: None,
        },
    );
    match read_response(&mut reader) {
        Response::SwapReport(report) => {
            assert!(!report.report.ok, "a bad document must not install");
            assert!(
                !report.report.errors.is_empty(),
                "the failure names its cause"
            );
            assert_eq!(
                report.content_hash, base_hash,
                "the prior hash still names what plays"
            );
            assert!(report.diff.is_none(), "a rejected swap has no diff");
        }
        other => panic!("expected SwapReport, got {other:?}"),
    }
    // Retain-prior: get_document still returns the original rig.
    send(&mut writer, &Request::GetDocument);
    match read_response(&mut reader) {
        Response::Document {
            content_hash: hash, ..
        } => assert_eq!(hash, base_hash),
        other => panic!("expected Document, got {other:?}"),
    }

    server.shutdown();
    cb.stop();
}

#[test]
fn swap_expect_arbitration_conflicts_then_proceeds_over_the_wire() {
    let base = envelope_doc("/env");
    let (state, cb, base_hash) = wired(&base, 0);
    let server = StructureServer::bind("127.0.0.1:0", state).expect("bind");
    let client = TcpStream::connect(server.local_addr()).expect("connect");
    let mut writer = client.try_clone().unwrap();
    let mut reader = BufReader::new(client);

    // A stale expect rejects with the real installed hash — nothing installs (ADR-0046 §9).
    let target: serde_json::Value = serde_json::from_str(&envelope_doc("/eg")).unwrap();
    send(
        &mut writer,
        &Request::Swap {
            source: DocSource::Document(target.clone()),
            expect: Some("0badc0de0badc0de".to_string()),
        },
    );
    match read_response(&mut reader) {
        Response::Conflict { expected, actual } => {
            assert_eq!(expected, "0badc0de0badc0de");
            assert_eq!(actual, base_hash, "conflict names the real installed hash");
        }
        other => panic!("expected Conflict, got {other:?}"),
    }

    // The matching expect (the real installed hash) proceeds.
    send(
        &mut writer,
        &Request::Swap {
            source: DocSource::Document(target),
            expect: Some(base_hash),
        },
    );
    match read_response(&mut reader) {
        Response::SwapReport(report) => {
            assert!(report.report.ok, "matching expect installs: {report:?}")
        }
        other => panic!("expected SwapReport, got {other:?}"),
    }

    server.shutdown();
    cb.stop();
}

#[test]
fn shuts_down_cleanly_with_an_idle_client_still_connected() {
    // A handler blocked on `read_line` for an idle client must not keep the server alive.
    let (state, cb, _) = wired(BASE_DOC, 0);
    let diagnostics = state.diagnostics();
    let server = StructureServer::bind("127.0.0.1:0", state).expect("bind");
    let _idle = TcpStream::connect(server.local_addr()).expect("connect");

    within(10, move || {
        server.shutdown();
        // The final exit-time snapshot flush `play` performs (ADR-0038 §9).
        reuben_native::diagnostics::log_snapshot(&diagnostics.snapshot());
    });
    cb.stop();
}

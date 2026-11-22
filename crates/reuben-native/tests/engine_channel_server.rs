//! Integration: the engine channel end-to-end over a real loopback TCP socket. Starts a
//! [`ChannelServer`] wired to a real Coordinator — everything `reuben play` wires up except
//! the cpal device (there is none in CI) — binds an **ephemeral** port (`127.0.0.1:0`,
//! OS-assigned, so parallel CI jobs never collide), then drives a plain `TcpStream` client
//! speaking NDJSON. The device half is stood in for by [`FakeCallback`] (`crate::test_support`,
//! shared with the unit tests so there is one mirror of the real callback, not two that can
//! diverge); the audible half — what a swap sounds like on a real device — is not covered here.
//!
//! Exercises: the verbs that read (ping, get_document, get_diagnostics, get_meters) over the wire
//! in order; `get_meters` reading the levels a callback metered; `open` installing via the
//! mailbox with real survivor stats and `expect` arbitration; an input-binding open onto an
//! output-only stream dark-degrading to silence with a warning; `send` converging at the engine's
//! `queue_osc`; and a clean shutdown with an idle client connected.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use reuben_api::engine::{
    credential_line, ChannelState, Conflict, ControlArg, ControlMessage, DiagnosticsReport,
    DocumentSnapshot, MetersReport, Request, Response, TcpTransport, Token, Transport,
    DEFAULT_CONNECT_TIMEOUT, MAX_LINE_BYTES, METER_FLOOR_DBFS, REFUSED_TOKEN,
};
use reuben_api::render::{Arg, AudioConfig};
use reuben_native::control::ring_pair;
use reuben_native::diagnostics::Diagnostics;
use reuben_native::engine_channel::{ChannelServer, HeadlessRenderConfig, NativeHost};
use reuben_native::meters::{Bank, Meters};
use reuben_native::osc::OscIn;
use reuben_native::test_support::{no_store, served, FakeCallback};

fn cfg() -> AudioConfig {
    AudioConfig::new(48_000.0, 128)
}

/// A minimal output-only rig: one oscillator through a master output. Self-contained (no
/// resources), so an empty store loads it.
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

/// A minimal instrument binding logical input channel 0 — live input an output-only
/// stream can't provide, the dark-degrade case.
const MIC_PASSTHRU: &str = r#"{ "format_version": 3, "instrument": "mic-passthru",
    "interface": {
        "inputs": { "mic": { "type": "f32_buffer", "channel": 0 } },
        "outputs": { "out": { "from": "/mic" } } },
    "nodes": [] }"#;

/// A Coordinator-backed [`ChannelState`] over `doc`, a live [`FakeCallback`] draining its mailbox
/// and its control ingress, and the base document's content hash. `opened_input_channels` is the
/// headless render config's input-stream geometry (`0` = output-only, so an input-binding swap
/// dark-degrades).
///
/// The control sink is wired exactly as `play` wires it — the engine-channel server on the producing end
/// of a control ring pair and the callback draining it. The harness is the crate's
/// [`FakeCallback`], shared with the unit tests, so one mirror of the real callback serves both.
fn wired(doc: &str, opened_input_channels: usize) -> (ChannelState, FakeCallback, String) {
    wired_watching(doc, opened_input_channels, Diagnostics::new())
}

/// [`wired`] with the counter surface passed in, for the one test that has to still hold a handle
/// to it after the host has taken its own — `play` holds one the same way.
fn wired_watching(
    doc: &str,
    opened_input_channels: usize,
    diagnostics: Arc<Diagnostics>,
) -> (ChannelState, FakeCallback, String) {
    let staged = no_store();
    let (coordinator, side, _warnings) =
        reuben_api::engine::install_initial(doc, staged.clone(), cfg()).expect("initial install");
    let base_hash = coordinator.installed_hash();
    let (control_tx, control_rx) = ring_pair(Diagnostics::new());
    let host = NativeHost::new(diagnostics, control_tx).with_render_config(Arc::new(
        HeadlessRenderConfig {
            opened_input_channels,
        },
    ));
    let state = ChannelState::new(coordinator, Arc::new(host), served(staged));
    (state, FakeCallback::spawn(side, control_rx), base_hash)
}

/// A directory the host offers, holding the documents an open names. Removed on drop.
struct Library(std::path::PathBuf);

impl Drop for Library {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Library {
    /// `text` written here as `file`, and the open that names it.
    fn open(&self, file: &str, text: &str, expect: Option<String>) -> Request {
        let path = self.0.join(file);
        std::fs::write(&path, text).expect("seed the document");
        Request::Open {
            name: self.name(file),
            expect,
        }
    }

    fn name(&self, file: &str) -> String {
        self.0.join(file).display().to_string()
    }
}

/// [`wired`] with `doc` playing from a [`Library`] the host offers and the server's store reads,
/// so a client can open what it writes there. `case` keeps concurrent tests apart.
fn wired_library(doc: &str, case: &str) -> (ChannelState, FakeCallback, String, Library) {
    use reuben_api::resources::Resources;
    use reuben_api::staged::Staged;
    use reuben_api::FsResolver;

    let dir = std::env::temp_dir().join(format!("reuben_wire_{case}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("case dir");
    let library = Library(dir);
    let top = library.0.join("base.json");
    std::fs::write(&top, doc).expect("seed what plays");
    let staged: Staged<Box<dyn Resources + Send + Sync>> =
        Staged::new(Box::new(FsResolver::for_instrument(&top)));
    let (coordinator, side, _warnings) =
        reuben_api::engine::install_initial(doc, staged.clone(), cfg()).expect("initial install");
    let base_hash = coordinator.installed_hash();
    let (control_tx, control_rx) = ring_pair(Diagnostics::new());
    let host = NativeHost::new(Diagnostics::new(), control_tx)
        .with_library_root(Some(library.0.clone()))
        .with_render_config(Arc::new(HeadlessRenderConfig {
            opened_input_channels: 0,
        }));
    let state = ChannelState::new(coordinator, Arc::new(host), served(staged))
        .with_installed_source(Some(top.display().to_string()));
    (
        state,
        FakeCallback::spawn(side, control_rx),
        base_hash,
        library,
    )
}

/// Read one newline-framed [`Response`] off the wire, asserting the framing.
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

/// The content hash `doc` should carry, computed **off the wire**: a second install of the same
/// document in this process, read back through the window.
///
/// Independent of the responses under test, which is the point — comparing a served hash against
/// another served hash only proves two answers agree, and would pass a channel that reported the
/// wrong document's hash consistently.
fn expected_hash(doc: &str) -> String {
    let (coordinator, _side, _warnings) =
        reuben_api::engine::install_initial(doc, no_store(), cfg())
            .expect("install for the expected hash");
    coordinator.installed_hash()
}

#[test]
fn serves_the_read_verbs_over_loopback_ndjson_in_order() {
    let (state, cb, base_hash) = wired(BASE_DOC, 0);
    let server =
        ChannelServer::bind("127.0.0.1:0", state).expect("bind loopback engine-channel port");
    let addr = server.local_addr();
    assert!(addr.ip().is_loopback(), "engine channel is loopback-only");

    let client = TcpStream::connect(addr).expect("connect to engine channel");
    let mut writer = client.try_clone().expect("clone client for writing");
    let mut reader = BufReader::new(client);

    send(&mut writer, &Request::Ping);
    assert_eq!(read_response(&mut reader), Response::Pong);

    send(&mut writer, &Request::GetDocument);
    match read_response(&mut reader) {
        Response::Document(DocumentSnapshot {
            content_hash: hash,
            source,
        }) => {
            assert_eq!(
                source, None,
                "this harness starts on a document with no file to name"
            );
            assert_eq!(hash, base_hash, "the snapshot carries the playing hash");
        }
        other => panic!("expected Document, got {other:?}"),
    }

    send(&mut writer, &Request::GetDiagnostics);
    assert_eq!(
        read_response(&mut reader),
        Response::Diagnostics(DiagnosticsReport::default())
    );

    // No device, so no channels to meter.
    send(&mut writer, &Request::GetMeters);
    assert_eq!(
        read_response(&mut reader),
        Response::Meters(MetersReport::default())
    );

    server.shutdown();
    cb.stop();
}

#[test]
fn get_meters_reads_the_levels_the_callbacks_metered() {
    let staged = no_store();
    let (coordinator, side, _w) =
        reuben_api::engine::install_initial(BASE_DOC, staged.clone(), cfg()).expect("install");
    let meters = Meters {
        outputs: Bank::new(2, 48_000.0),
        inputs: Bank::new(1, 48_000.0),
    };
    let (control_tx, control_rx) = ring_pair(Diagnostics::new());
    let state = ChannelState::new(
        coordinator,
        Arc::new(NativeHost::new(Diagnostics::new(), control_tx).with_meters(meters.clone())),
        served(staged),
    );
    let cb = FakeCallback::spawn(side, control_rx);
    let server = ChannelServer::bind("127.0.0.1:0", state).expect("bind");
    let client = TcpStream::connect(server.local_addr()).expect("connect");
    let mut writer = client.try_clone().unwrap();
    let mut reader = BufReader::new(client);

    // Left at full scale, right silent; the input at half scale.
    let block: Vec<f32> = (0..128).flat_map(|_| [1.0, 0.0]).collect();
    meters.outputs.meter(&block, 128);
    meters.inputs.meter(&[0.5; 128], 128);

    send(&mut writer, &Request::GetMeters);
    let Response::Meters(report) = read_response(&mut reader) else {
        panic!("expected Meters");
    };
    assert_eq!(report.frames, 128);
    assert_eq!(report.outputs.len(), 2);
    assert_eq!(report.outputs[0].peak_dbfs, 0.0);
    assert_eq!(report.outputs[1].peak_dbfs, METER_FLOOR_DBFS);
    assert_eq!(report.inputs.len(), 1);
    assert!(
        (report.inputs[0].peak_dbfs + 6.02).abs() < 0.01,
        "{report:?}"
    );

    server.shutdown();
    cb.stop();
}

#[test]
fn responses_come_back_one_per_request_in_pipelined_order() {
    let (state, cb, _) = wired(BASE_DOC, 0);
    let server = ChannelServer::bind("127.0.0.1:0", state).expect("bind");
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
    assert!(matches!(read_response(&mut reader), Response::Document(_)));

    server.shutdown();
    cb.stop();
}

#[test]
fn get_diagnostics_reflects_live_counter_bumps() {
    // The endpoint reads the live Arc audio::start owns, not a copy frozen at startup.
    let staged = no_store();
    let (coordinator, side, _w) =
        reuben_api::engine::install_initial(BASE_DOC, staged.clone(), cfg()).expect("install");
    let diagnostics = Diagnostics::new();
    // This verb never touches control, but the ingress is a constructor parameter now.
    let (control_tx, control_rx) = ring_pair(Diagnostics::new());
    let state = ChannelState::new(
        coordinator,
        Arc::new(NativeHost::new(Arc::clone(&diagnostics), control_tx)),
        served(staged),
    );
    let cb = FakeCallback::spawn(side, control_rx);
    let server = ChannelServer::bind("127.0.0.1:0", state).expect("bind");
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
fn open_over_the_wire_installs_via_the_mailbox_with_real_survivor_stats() {
    // An open of an identical envelope document installs over the wire through the
    // mailbox — the FakeCallback drains it (its `reclaim` completing is the proof, and no
    // stream restart is invoked) — and the real diff carries `survived: 2`
    // (both nodes), impossible under an all-cold restart. `get_document` then names the new one.
    let base = envelope_doc("/env");
    let (state, cb, base_hash, library) = wired_library(&base, "survivors");
    let server = ChannelServer::bind("127.0.0.1:0", state).expect("bind");
    let client = TcpStream::connect(server.local_addr()).expect("connect");
    let mut writer = client.try_clone().unwrap();
    let mut reader = BufReader::new(client);

    send(&mut writer, &library.open("same.json", &base, None));
    match read_response(&mut reader) {
        Response::SwapReport(report) => {
            assert!(report.report.ok, "a valid document installs: {report:?}");
            let diff = report.diff.expect("a successful open carries a diff");
            assert_eq!(
                diff.survived, 2,
                "both nodes survive an identical-document swap (real migration): {diff:?}"
            );
            assert_eq!(report.content_hash, base_hash, "same doc -> same hash");
        }
        other => panic!("expected SwapReport, got {other:?}"),
    }

    // A second, renamed open: only `/out` survives — proves back-to-back opens turn over through
    // the mailbox (the first's retiree was reclaimed) with real, changing survivor stats.
    send(
        &mut writer,
        &library.open("eg.json", &envelope_doc("/eg"), None),
    );
    let renamed_hash = match read_response(&mut reader) {
        Response::SwapReport(report) => {
            assert!(report.report.ok, "{report:?}");
            let diff = report.diff.expect("diff");
            assert_eq!(diff.survived, 1, "only /out survives a rename: {diff:?}");
            report.content_hash
        }
        other => panic!("expected SwapReport, got {other:?}"),
    };

    // get_document names the last opened document.
    send(&mut writer, &Request::GetDocument);
    match read_response(&mut reader) {
        Response::Document(DocumentSnapshot {
            content_hash: hash,
            source,
            ..
        }) => {
            assert_eq!(source, Some(library.name("eg.json")));
            assert_eq!(hash, expected_hash(&envelope_doc("/eg")));
            // …and the open that installed it reported that same hash, so the two responses agree
            // with each other *and* with what the document actually hashes to.
            assert_eq!(hash, renamed_hash);
            assert_ne!(hash, base_hash, "the open changed the installed document");
        }
        other => panic!("expected Document, got {other:?}"),
    }

    server.shutdown();
    cb.stop();
}

#[test]
fn input_binding_open_over_the_wire_dark_degrades_and_stays_alive() {
    // The initial rig is output-only (no input stream). An open of an instrument
    // that binds an input channel installs and stays silent-but-alive — a loud swap-report WARNING,
    // never an error or a crash. The FakeCallback keeps rendering (the process stays alive).
    let (state, cb, _, library) = wired_library(BASE_DOC, "dark_degrade");
    let server = ChannelServer::bind("127.0.0.1:0", state).expect("bind");
    let client = TcpStream::connect(server.local_addr()).expect("connect");
    let mut writer = client.try_clone().unwrap();
    let mut reader = BufReader::new(client);

    send(&mut writer, &library.open("mic.json", MIC_PASSTHRU, None));
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

    // Still alive afterward: the channel answers, and the mic doc is what's open.
    send(&mut writer, &Request::GetDocument);
    match read_response(&mut reader) {
        Response::Document(DocumentSnapshot { source, .. }) => {
            assert_eq!(source, Some(library.name("mic.json")))
        }
        other => panic!("expected Document, got {other:?}"),
    }

    server.shutdown();
    cb.stop();
}

#[test]
fn open_expect_arbitration_conflicts_then_proceeds_over_the_wire() {
    let base = envelope_doc("/env");
    let (state, cb, base_hash, library) = wired_library(&base, "expect");
    let server = ChannelServer::bind("127.0.0.1:0", state).expect("bind");
    let client = TcpStream::connect(server.local_addr()).expect("connect");
    let mut writer = client.try_clone().unwrap();
    let mut reader = BufReader::new(client);

    // A stale expect rejects with the real installed hash — nothing installs.
    send(
        &mut writer,
        &library.open(
            "eg.json",
            &envelope_doc("/eg"),
            Some("0badc0de0badc0de".to_string()),
        ),
    );
    match read_response(&mut reader) {
        Response::Conflict(Conflict { expected, actual }) => {
            assert_eq!(expected, "0badc0de0badc0de");
            assert_eq!(actual, base_hash, "conflict names the real installed hash");
        }
        other => panic!("expected Conflict, got {other:?}"),
    }

    // The matching expect (the real installed hash) proceeds.
    send(
        &mut writer,
        &library.open("eg.json", &envelope_doc("/eg"), Some(base_hash)),
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
fn send_over_the_wire_reaches_the_engines_queue_osc() {
    // Control end-to-end over real loopback TCP: one `send` verb carries the whole batch,
    // the ack names what the engine queued, and the messages arrive at `queue_osc` — the same call
    // a decoded external OSC datagram reaches. That convergence is the point of the verb: the
    // sidecar ships `{address, [Arg]}` in this channel's framing and OSC-the-binary-protocol never
    // enters the loop.
    let (state, cb, _) = wired(BASE_DOC, 0);
    let server = ChannelServer::bind("127.0.0.1:0", state).expect("bind");
    let client = TcpStream::connect(server.local_addr()).expect("connect");
    let mut writer = client.try_clone().unwrap();
    let mut reader = BufReader::new(client);

    // Distinct atom kinds so a mis-mapped variant cannot pass unnoticed.
    send(
        &mut writer,
        &Request::Send {
            messages: vec![
                ControlMessage {
                    address: "/osc/freq".to_string(),
                    args: vec![ControlArg::F32(440.5)],
                },
                ControlMessage {
                    address: "/osc/shape".to_string(),
                    args: vec![ControlArg::Str("Up".to_string()), ControlArg::I32(3)],
                },
            ],
        },
    );
    assert_eq!(read_response(&mut reader), Response::Sent);

    assert_eq!(
        cb.await_queued(2),
        vec![
            OscIn {
                address: "/osc/freq".to_string(),
                args: vec![Arg::F32(440.5)],
            },
            OscIn {
                address: "/osc/shape".to_string(),
                args: vec![Arg::Str("Up".into()), Arg::I32(3)],
            },
        ],
        "both messages crossed the wire in order, each atom as its primitive Arg"
    );

    // Control and structure share the channel without interfering: the connection is still good
    // for a structure verb right after.
    send(&mut writer, &Request::Ping);
    assert_eq!(read_response(&mut reader), Response::Pong);

    server.shutdown();
    cb.stop();
}

#[test]
fn concurrent_sends_never_interleave_into_each_others_gestures() {
    // The atomicity the wire promises, under the condition that breaks a naive implementation:
    // `accept_loop` runs one handler thread per connection, so three clients dispatch `send`s truly
    // concurrently. Pushing message-by-message would let their gestures interleave on the shared
    // ingress (A1, B1, A2, …) — the multi-client workflow is explicitly supported, so this is not a
    // hypothetical. Handing each batch over as ONE unit is what prevents it.
    let (state, cb, _) = wired(BASE_DOC, 0);
    let server = ChannelServer::bind("127.0.0.1:0", state).expect("bind");
    let addr = server.local_addr();

    const CLIENTS: usize = 3;
    const GESTURES: usize = 20;
    const PER_GESTURE: usize = 8;

    let threads: Vec<_> = ["a", "b", "c"]
        .iter()
        .map(|who| {
            let who = who.to_string();
            std::thread::spawn(move || {
                let client = TcpStream::connect(addr).expect("connect");
                let mut writer = client.try_clone().expect("clone");
                let mut reader = BufReader::new(client);
                for _ in 0..GESTURES {
                    let request = Request::Send {
                        messages: (0..PER_GESTURE)
                            .map(|i| ControlMessage {
                                // The address's leading letter tags which client owns the message,
                                // so a mixed batch is detectable below.
                                address: format!("/{who}/p{i}"),
                                args: vec![ControlArg::I32(i as i32)],
                            })
                            .collect(),
                    };
                    send(&mut writer, &request);
                    assert_eq!(read_response(&mut reader), Response::Sent);
                }
            })
        })
        .collect();
    for t in threads {
        t.join().expect("client thread");
    }

    let total = CLIENTS * GESTURES * PER_GESTURE;
    let queued = cb.await_queued(total);
    assert_eq!(queued.len(), total, "every message arrived");

    // Every drained batch is exactly one gesture: a short batch would mean one was split, a long
    // one that two got merged into a single drain slice. Either is an interleave.
    let sizes = cb.drained_batch_sizes();
    assert!(
        sizes.iter().all(|&n| n == PER_GESTURE),
        "a gesture was split or merged on the ingress: {sizes:?}"
    );
    assert_eq!(
        sizes.len(),
        CLIENTS * GESTURES,
        "one batch per gesture, no more and no fewer"
    );

    // And each batch belongs start-to-finish to one client.
    for chunk in queued.chunks(PER_GESTURE) {
        let owners: Vec<&str> = chunk
            .iter()
            .map(|m| m.address.split('/').nth(1).expect("owner segment"))
            .collect();
        assert!(
            owners.iter().all(|o| *o == owners[0]),
            "one batch carried two clients' messages: {owners:?}"
        );
    }

    server.shutdown();
    cb.stop();
}

#[test]
fn shuts_down_cleanly_with_an_idle_client_still_connected() {
    // A handler blocked on `read_line` for an idle client must not keep the server alive.
    // `play` holds its own handle to the counter surface — the host takes a clone — so the
    // exit-time flush below outlives the state the server consumed.
    let diagnostics = Diagnostics::new();
    let (state, cb, _) = wired_watching(BASE_DOC, 0, Arc::clone(&diagnostics));
    let server = ChannelServer::bind("127.0.0.1:0", state).expect("bind");
    let _idle = TcpStream::connect(server.local_addr()).expect("connect");

    within(10, move || {
        server.shutdown();
        // The final exit-time snapshot flush `play` performs.
        reuben_native::diagnostics::log_snapshot(&diagnostics.snapshot());
    });
    cb.stop();
}

#[test]
fn a_request_line_past_the_cap_is_answered_with_an_error_and_the_connection_closed() {
    let (state, cb, _) = wired(BASE_DOC, 0);
    let server = ChannelServer::bind("127.0.0.1:0", state).expect("bind");
    let client = TcpStream::connect(server.local_addr()).expect("connect");
    let mut writer = client.try_clone().unwrap();
    let mut reader = BufReader::new(client);

    // Every byte the cap allows, and no newline: the server can tell it is too long without
    // reading further, so this write is never left waiting on it.
    within(10, move || {
        writer
            .write_all(&vec![b'x'; MAX_LINE_BYTES])
            .expect("the server reads up to the cap");
    });
    match read_response(&mut reader) {
        Response::Error { message } => {
            assert!(
                message.contains(&format!("longer than {MAX_LINE_BYTES} bytes")),
                "{message}"
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }
    let mut rest = String::new();
    assert_eq!(
        reader.read_line(&mut rest).expect("read after the error"),
        0,
        "the server closes a connection whose line boundary it lost"
    );

    server.shutdown();
    cb.stop();
}

/// What a client built before `swap` was removed gets for one: the line is an unreadable request,
/// nothing installs, and the connection goes on answering.
#[test]
fn a_swap_line_is_unreadable_and_the_connection_keeps_serving() {
    let (state, cb, base_hash, library) = wired_library(BASE_DOC, "old_swap");
    std::fs::write(library.0.join("x.json"), envelope_doc("/eg")).expect("seed the target");
    let server = ChannelServer::bind("127.0.0.1:0", state).expect("bind");
    let client = TcpStream::connect(server.local_addr()).expect("connect");
    let mut writer = client.try_clone().unwrap();
    let mut reader = BufReader::new(client);

    let old = serde_json::json!({
        "verb": "swap",
        "source": { "path": library.name("x.json") }
    });
    writer
        .write_all(format!("{old}\n").as_bytes())
        .expect("send the old line");
    match read_response(&mut reader) {
        Response::Error { message } => {
            assert!(message.starts_with("unreadable request"), "{message}")
        }
        other => panic!("expected Error, got {other:?}"),
    }

    send(&mut writer, &Request::Ping);
    assert_eq!(read_response(&mut reader), Response::Pong);
    let fresh = TcpStream::connect(server.local_addr()).expect("connect again");
    let mut fresh_writer = fresh.try_clone().unwrap();
    send(&mut fresh_writer, &Request::Ping);
    assert_eq!(read_response(&mut BufReader::new(fresh)), Response::Pong);

    send(&mut writer, &Request::GetDocument);
    match read_response(&mut reader) {
        Response::Document(DocumentSnapshot { content_hash, .. }) => {
            assert_eq!(content_hash, base_hash, "nothing installed")
        }
        other => panic!("expected Document, got {other:?}"),
    }

    server.shutdown();
    cb.stop();
}

/// A token for the tests that serve beyond loopback.
fn token(t: &str) -> Token {
    Token::new(t).expect("a token")
}

/// One ping over a fresh connection from a transport carrying `token`.
fn ping_with(addr: std::net::SocketAddr, token: Option<Token>) -> Response {
    let transport = TcpTransport::new(addr.to_string(), DEFAULT_CONNECT_TIMEOUT).with_token(token);
    let line = transport
        .round_trip(&Request::Ping.to_ndjson(), Duration::from_secs(5))
        .expect("an answer");
    Response::from_ndjson(&line).expect("a response")
}

/// The loopback address of a server bound on every interface.
fn on_loopback(server: &ChannelServer) -> std::net::SocketAddr {
    std::net::SocketAddr::from(([127, 0, 0, 1], server.local_addr().port()))
}

#[test]
fn a_bind_beyond_loopback_without_a_token_is_refused() {
    let (state, cb, _) = wired(BASE_DOC, 0);
    let refused = ChannelServer::bind_with_token("0.0.0.0:0", state, None)
        .err()
        .expect("refused");
    assert_eq!(
        refused.kind(),
        std::io::ErrorKind::InvalidInput,
        "{refused}"
    );
    cb.stop();
}

#[test]
fn a_loopback_client_needs_no_token_on_a_server_that_has_one() {
    let (state, cb, _) = wired(BASE_DOC, 0);
    let server = ChannelServer::bind_with_token("0.0.0.0:0", state, Some(token("good")))
        .expect("bind with a token");
    assert_eq!(ping_with(on_loopback(&server), None), Response::Pong);
    assert_eq!(
        ping_with(on_loopback(&server), Some(token("good"))),
        Response::Pong
    );
    server.shutdown();
    cb.stop();
}

#[test]
fn a_wrong_token_is_refused_even_from_loopback() {
    let (state, cb, _) = wired(BASE_DOC, 0);
    let server = ChannelServer::bind_with_token("0.0.0.0:0", state, Some(token("good")))
        .expect("bind with a token");
    assert_eq!(
        ping_with(on_loopback(&server), Some(token("bad"))),
        Response::Error {
            message: REFUSED_TOKEN.to_string()
        }
    );
    assert_eq!(
        ping_with(on_loopback(&server), Some(token("good"))),
        Response::Pong
    );
    server.shutdown();
    cb.stop();
}

#[test]
fn a_credential_after_the_first_line_is_unreadable() {
    let (state, cb, _) = wired(BASE_DOC, 0);
    let server = ChannelServer::bind_with_token("0.0.0.0:0", state, Some(token("good")))
        .expect("bind with a token");
    let client = TcpStream::connect(on_loopback(&server)).expect("connect");
    let mut writer = client.try_clone().expect("clone");
    let mut reader = BufReader::new(client);
    send(&mut writer, &Request::Ping);
    assert_eq!(read_response(&mut reader), Response::Pong);
    writer
        .write_all(credential_line(&token("good")).as_bytes())
        .expect("write the credential");
    match read_response(&mut reader) {
        Response::Error { message } => {
            assert!(message.starts_with("unreadable request"), "{message}")
        }
        other => panic!("expected an unreadable request, got {other:?}"),
    }
    server.shutdown();
    cb.stop();
}

#[test]
fn a_server_without_a_token_ignores_a_credential() {
    let (state, cb, _) = wired(BASE_DOC, 0);
    let server = ChannelServer::bind("127.0.0.1:0", state).expect("bind");
    assert_eq!(
        ping_with(server.local_addr(), Some(token("any"))),
        Response::Pong
    );
    server.shutdown();
    cb.stop();
}

/// A refused client whose request reaches the server after it answered still reads the refusal:
/// the server does not reset the connection under it.
#[test]
fn a_refusal_survives_a_request_that_arrives_after_it() {
    let (state, cb, _) = wired(BASE_DOC, 0);
    let server = ChannelServer::bind_with_token("0.0.0.0:0", state, Some(token("good")))
        .expect("bind with a token");
    let client = TcpStream::connect(on_loopback(&server)).expect("connect");
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("a read timeout");
    let mut writer = client.try_clone().expect("clone");
    writer
        .write_all(credential_line(&token("bad")).as_bytes())
        .expect("write the credential");
    std::thread::sleep(Duration::from_millis(300));
    let _ = writer.write_all(Request::Ping.to_ndjson().as_bytes());
    std::thread::sleep(Duration::from_millis(300));
    let mut reader = BufReader::new(client);
    assert_eq!(
        read_response(&mut reader),
        Response::Error {
            message: REFUSED_TOKEN.to_string()
        }
    );
    server.shutdown();
    cb.stop();
}

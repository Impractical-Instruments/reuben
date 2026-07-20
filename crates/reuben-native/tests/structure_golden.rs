//! Golden-pinned live-server integration test for the structure channel.
//!
//! The companion to `structure_server.rs`: where that test asserts the channel's *behavior*
//! field by field, this one pins the **exact NDJSON wire bytes** each verb answers with as
//! golden fixtures under `tests/golden/`, gated on a `REUBEN_BLESS=1` bless convention so any
//! wire-format drift — a renamed field, a reordered key, a changed hash or diff — reds CI.
//!
//! It starts the **real** structure-channel server in-process ([`StructureServer::bind`] on an
//! ephemeral `127.0.0.1:0` port) — everything `reuben play` wires up except the cpal device.
//! M2 (#323) drives the swap through the real Coordinator/mailbox path; a background **fake audio
//! callback** owns the [`RenderSlot`] and drains that mailbox, so a swap installs via
//! the mailbox and its off-thread reclaim completes with no device. A raw [`TcpStream`] client
//! drives all four verbs (`ping` / `swap` / `get_document` / `get_diagnostics`) against a **canned
//! document**; every response is captured off the wire, its framing asserted, its contract invariant
//! checked, and its raw bytes pinned as a golden fixture.
//!
//! The M2 `swap`-verb contract is covered headlessly here: a validation **success** (`ok:true` with
//! **real** survivor stats — the `/osc`→`/sub` rename keeps `/out` a survivor, so `survived:1`, not
//! M1's blanket `0`), a validation **failure** (an ordinary `SwapReport` with `ok:false`, *not* a
//! transport error), and the `expect` **conflict** path (`Conflict{expected, actual}`).
//! The device-level gapless swap is the scripted human ritual
//! `docs/rituals/m2-swap-ramp-duck.md`. To re-bless after a deliberate wire change:
//! `REUBEN_BLESS=1 cargo test -p reuben-native --test structure_golden`.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use reuben_core::coordinator::{Coordinator, DocSource, Request, Response};
use reuben_core::coordinator::{RenderSide, RenderSlot};
use reuben_core::resources::MemoryResolver;
use reuben_core::{AudioConfig, Registry};
use reuben_native::diagnostics::Diagnostics;
use reuben_native::structure::{StructureServer, StructureState};

/// The fixed canned starting document the server retains — a bare 110 Hz oscillator through a
/// master output. Minimal and resource-free so its normalized form (and thus its `content_hash`,
/// which every golden below embeds) is small and stable; the maps a document carries serialize
/// in sorted (`BTreeMap`) order, so the pinned bytes are deterministic run to run.
const CANNED: &str = r#"{
    "format_version": 3,
    "instrument": "m1-canned",
    "interface": { "outputs": { "main": { "from": "/out.audio" } } },
    "nodes": [
        { "type": "oscillator", "address": "/osc", "inputs": { "freq": 110.0 } },
        { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/osc.audio" } } }
    ]
}"#;

/// The fixed document a successful `swap` installs — a 55 Hz oscillator (`/sub`) through the same
/// `/out`. Deliberately *renames* the oscillator (`/osc` → `/sub`) so the **real** migration diff
/// exercises all three buckets: `/out` matches on address + type + fingerprint → **survivor**,
/// `/sub` is new (`added`), `/osc` is gone (`removed`) — real survivor stats, the
/// whole-document re-emission accidents the swap report wants surfaced.
const SWAP_TARGET: &str = r#"{
    "format_version": 3,
    "instrument": "m1-swapped",
    "interface": { "outputs": { "main": { "from": "/out.audio" } } },
    "nodes": [
        { "type": "oscillator", "address": "/sub", "inputs": { "freq": 55.0 } },
        { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/sub.audio" } } }
    ]
}"#;

/// A document that fails to load — an unknown operator type — so validation rejects it. Its
/// failure is an ordinary `SwapReport` with `ok:false` (the channel *working*), never a
/// channel-level `Error`.
const BAD_DOC: &str = r#"{
    "format_version": 3,
    "instrument": "m1-bad",
    "nodes": [ { "type": "no_such_operator", "address": "/x" } ]
}"#;

/// A hash the client wrongly believes is installed — drives the `expect` conflict path. Never
/// matches the canned document's real hash, so the swap is rejected with a `Conflict` that names
/// the real installed hash and installs nothing.
const STALE_EXPECT: &str = "0badc0de0badc0de";

/// A background "fake audio callback": owns the [`RenderSlot`] the real cpal callback
/// would and drives it in a loop, draining the install mailbox — so a swap installs via the mailbox
/// and the Coordinator's off-thread `reclaim` completes, with no audio device.
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

/// Absolute path to a golden fixture under this crate's `tests/golden/` tree.
fn golden_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

/// Pin `actual` (the raw wire bytes of one response) against `tests/golden/<name>`, gated on the
/// `REUBEN_BLESS` env var: set it to regenerate, unset to assert byte-equality. A missing golden
/// points the reader at the bless command instead of a bare file-not-found.
fn assert_golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if std::env::var_os("REUBEN_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().expect("golden dir has a parent"))
            .expect("create golden dir");
        std::fs::write(&path, actual).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing golden {}: {e}\nfirst run: REUBEN_BLESS=1 cargo test -p reuben-native --test structure_golden",
            path.display()
        )
    });
    assert_eq!(
        actual, expected,
        "wire response for {name} drifted from the golden snapshot. \
         If this change is intentional, re-bless with REUBEN_BLESS=1."
    );
}

/// Build the canned starting [`StructureState`] the server serves — a real [`Coordinator`] over
/// the canned document (exactly as `play` builds it) — plus the [`FakeCallback`]
/// draining its mailbox so a swap installs via the mailbox headlessly.
fn canned_wired() -> (StructureState, FakeCallback) {
    let (coordinator, side, _warnings) = Coordinator::install_initial(
        CANNED,
        Registry::builtin(),
        Box::new(MemoryResolver::new()),
        AudioConfig::new(48_000.0, 128),
    )
    .expect("the canned document installs");
    let state = StructureState::from_coordinator(coordinator, Diagnostics::new());
    (state, FakeCallback::spawn(side))
}

fn send(writer: &mut impl Write, req: &Request) {
    writer
        .write_all(req.to_ndjson().as_bytes())
        .expect("send request");
}

/// Read one newline-framed response off the wire, asserting the NDJSON framing (one
/// JSON object per line, newline-terminated, one response per request), and return the **raw**
/// line — trailing newline included — so the golden pins the exact bytes, framing and all.
fn read_line_raw(reader: &mut impl BufRead) -> String {
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
    line
}

/// Send `req`, read its framed reply, and return both the raw wire bytes (to pin) and the parsed
/// [`Response`] (to assert the contract invariant on), so each exchange both byte-pins the wire
/// and reads as a spec.
fn exchange(
    writer: &mut impl Write,
    reader: &mut impl BufRead,
    req: &Request,
) -> (String, Response) {
    send(writer, req);
    let raw = read_line_raw(reader);
    let parsed = Response::from_ndjson(&raw).expect("a response parses as JSON");
    (raw, parsed)
}

/// The one live-server transcript: bind the real server on an ephemeral loopback port, drive all
/// four verbs over a raw TCP client against the canned document, and pin every response's exact
/// wire bytes. The sequence is ordered so the two rejecting swaps (validation failure, stale
/// `expect`) run against the *unmutated* canned document — both retain-prior, so `get_document`'s
/// pinned bytes stay the canned doc — and the one committing swap runs last.
#[test]
fn live_server_wire_responses_match_golden() {
    let (state, callback) = canned_wired();
    let server = StructureServer::bind("127.0.0.1:0", state).expect("bind loopback structure port");
    assert!(
        server.local_addr().ip().is_loopback(),
        "structure channel is loopback-only"
    );
    let client = TcpStream::connect(server.local_addr()).expect("connect to structure channel");
    let mut writer = client.try_clone().expect("clone client for writing");
    let mut reader = BufReader::new(client);

    // ping -> pong: the channel proves itself alive.
    let (raw, resp) = exchange(&mut writer, &mut reader, &Request::Ping);
    assert_eq!(resp, Response::Pong);
    assert_golden("ping.ndjson", &raw);

    // get_document -> the retained canned document + its content hash.
    let (raw, resp) = exchange(&mut writer, &mut reader, &Request::GetDocument);
    match &resp {
        Response::Document { document, .. } => {
            assert_eq!(document["instrument"], serde_json::json!("m1-canned"));
        }
        other => panic!("expected Document, got {other:?}"),
    }
    assert_golden("get_document.ndjson", &raw);

    // get_diagnostics -> a zeroed snapshot at startup.
    let (raw, _) = exchange(&mut writer, &mut reader, &Request::GetDiagnostics);
    assert_golden("get_diagnostics.ndjson", &raw);

    // swap of a bad document -> validation FAILURE: an ordinary SwapReport with ok:false and the
    // prior hash retained, not a channel Error. Nothing installs.
    let bad: serde_json::Value = serde_json::from_str(BAD_DOC).expect("bad doc parses");
    let (raw, resp) = exchange(
        &mut writer,
        &mut reader,
        &Request::Swap {
            source: DocSource::Document(bad),
            expect: None,
        },
    );
    match &resp {
        Response::SwapReport(report) => {
            assert!(!report.report.ok, "a bad document must not install");
            assert!(
                !report.report.errors.is_empty(),
                "the failure names its cause"
            );
            assert!(report.diff.is_none(), "a rejected swap has no diff");
        }
        other => panic!("expected SwapReport, got {other:?}"),
    }
    assert_golden("swap_validation_failure.ndjson", &raw);

    // swap with a stale expect -> CONFLICT naming the real installed hash; nothing installs.
    // Still against the unmutated canned document.
    let target: serde_json::Value = serde_json::from_str(SWAP_TARGET).expect("target parses");
    let (raw, resp) = exchange(
        &mut writer,
        &mut reader,
        &Request::Swap {
            source: DocSource::Document(target.clone()),
            expect: Some(STALE_EXPECT.to_string()),
        },
    );
    match &resp {
        Response::Conflict { expected, .. } => assert_eq!(expected, STALE_EXPECT),
        other => panic!("expected Conflict, got {other:?}"),
    }
    assert_golden("swap_conflict.ndjson", &raw);

    // swap the whole document -> SUCCESS via the mailbox: ok:true, the new content hash, and a diff
    // with **real** survivor stats — `/out` survives the `/osc`→`/sub` rename, so `survived:1`,
    // not M1's blanket `0`. This one commits.
    let (raw, resp) = exchange(
        &mut writer,
        &mut reader,
        &Request::Swap {
            source: DocSource::Document(target),
            expect: None,
        },
    );
    match &resp {
        Response::SwapReport(report) => {
            assert!(report.report.ok, "a valid document installs: {report:?}");
            let diff = report
                .diff
                .as_ref()
                .expect("a successful swap carries a diff");
            assert_eq!(
                diff.survived, 1,
                "the mailbox swap keeps /out a survivor across the /osc->/sub rename"
            );
            assert_eq!(diff.removed, vec!["/osc".to_string()], "diff: {diff:?}");
            assert_eq!(diff.added, vec!["/sub".to_string()], "diff: {diff:?}");
            assert!(diff.state_reset.is_empty(), "diff: {diff:?}");
        }
        other => panic!("expected SwapReport, got {other:?}"),
    }
    assert_golden("swap_success.ndjson", &raw);

    server.shutdown();
    callback.stop();
}

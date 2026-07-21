//! Integration test for the reuben-mcp structure-channel client (#315 verification): stand up a
//! minimal loopback NDJSON stub speaking the shared `reuben_core::coordinator` wire envelope, and
//! drive the client's four verbs over the real TCP boundary — the same socket a live `reuben play`
//! server presents. `ping` returns Pong; `swap` (by value AND by path) and `get_document`
//! round-trip a document; a connect against a dead port fails fast with the "start `reuben play`"
//! guidance, not a hang or panic. Every case is bounded by a watchdog so a wedged
//! client fails loudly instead of hanging CI.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use reuben_core::coordinator::{
    Conflict, DiagnosticsReport, DocSource, DocumentSnapshot, Request, Response,
};
use reuben_core::{Diag, DiffSummary, Report, SwapReport};

use reuben_mcp::{default_osc_addr, EngineLink, StructureClient, SwapOutcome};

/// A running loopback NDJSON stub: the address the client dials, plus a receiver of every request
/// the stub parsed off the wire (so a test can assert the client sent the *right* envelope — e.g.
/// a by-path vs by-value swap). Mirrors the real server's framing: one `Response` per `Request`
/// line, in order, connection kept open until the client closes it.
struct Stub {
    addr: SocketAddr,
    requests: mpsc::Receiver<Request>,
}

/// Spawn a loopback stub that answers each parsed request with `responder(&req)`. The accept loop
/// is detached (it blocks forever on `accept` after the last exchange); the test process reaping
/// on exit is the stub's only shutdown, which is fine for a bounded unit test.
fn spawn_stub(responder: impl Fn(&Request) -> Response + Send + 'static) -> Stub {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback stub");
    let addr = listener.local_addr().expect("stub local addr");
    let (req_tx, requests) = mpsc::channel();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let _ = stream.set_nodelay(true);
            let mut reader = BufReader::new(stream.try_clone().expect("clone stub stream"));
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break, // client closed the connection
                    Ok(_) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        let req = Request::from_ndjson(&line).expect("stub parses a request line");
                        let resp = responder(&req);
                        let _ = req_tx.send(req);
                        (&stream)
                            .write_all(resp.to_ndjson().as_bytes())
                            .expect("stub writes response");
                        (&stream).flush().expect("stub flushes response");
                    }
                    Err(_) => break,
                }
            }
        }
    });
    Stub { addr, requests }
}

/// Run `f` on a helper thread and fail loudly if it does not finish within `dur` — the client's
/// own connect/read timeouts should make every call return, so a blown watchdog means a hang.
fn within<T: Send + 'static>(
    dur: Duration,
    label: &'static str,
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(dur) {
        Ok(v) => v,
        Err(_) => panic!("{label} did not complete within {dur:?} — the client hung"),
    }
}

/// An address with nothing listening: bind an ephemeral port, read it back, then drop the listener
/// so the port is free again. Connecting to it is refused immediately (fail-fast, no timeout wait).
fn dead_addr() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind to reserve a port");
    let addr = listener.local_addr().expect("reserved addr");
    drop(listener);
    addr.to_string()
}

/// A fully-populated report, so a round-trip proves the client carries the contract type intact.
fn sample_swap_report() -> SwapReport {
    SwapReport {
        report: Report {
            ok: true,
            errors: vec![],
            warnings: vec![Diag {
                node: Some("/voicer".to_string()),
                port: None,
                message: "missing resource".to_string(),
            }],
        },
        content_hash: "00c0ffee00c0ffee".to_string(),
        diff: Some(DiffSummary {
            survived: 2,
            state_reset: vec!["/osc".to_string()],
            added: vec![],
            removed: vec![],
        }),
    }
}

#[test]
fn ping_returns_pong() {
    let stub = spawn_stub(|req| match req {
        Request::Ping => Response::Pong,
        other => Response::Error {
            message: format!("unexpected {other:?}"),
        },
    });
    let client = StructureClient::new(stub.addr.to_string());
    within(Duration::from_secs(5), "ping", move || {
        client.ping().expect("ping resolves to Pong");
    });
}

#[test]
fn swap_by_value_round_trips_a_document() {
    let report = sample_swap_report();
    let expected = report.clone();
    let stub = spawn_stub(move |_req| Response::SwapReport(report.clone()));
    let client = StructureClient::new(stub.addr.to_string());

    let doc = serde_json::json!({ "format_version": 3, "instrument": "t", "nodes": [] });
    let outcome = within(Duration::from_secs(5), "swap by value", {
        let doc = doc.clone();
        move || {
            client
                .swap(DocSource::Document(doc), None)
                .expect("swap resolves to a report")
        }
    });
    assert_eq!(outcome, SwapOutcome::Installed(expected));

    // The client sent the inline-document branch, expect omitted.
    let sent = stub.requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(
        sent,
        Request::Swap {
            source: DocSource::Document(doc),
            expect: None,
        }
    );
}

#[test]
fn swap_by_path_round_trips_with_expect_guard() {
    let report = sample_swap_report();
    let expected = report.clone();
    let stub = spawn_stub(move |_req| Response::SwapReport(report.clone()));
    let client = StructureClient::new(stub.addr.to_string());

    let outcome = within(Duration::from_secs(5), "swap by path", move || {
        client
            .swap(
                DocSource::Path("instruments/warm-pad.json".to_string()),
                Some("00c0ffee00c0ffee".to_string()),
            )
            .expect("swap resolves to a report")
    });
    assert_eq!(outcome, SwapOutcome::Installed(expected));

    // Both branches of DocSource are accepted; the client sent the by-path form with the guard.
    let sent = stub.requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(
        sent,
        Request::Swap {
            source: DocSource::Path("instruments/warm-pad.json".to_string()),
            expect: Some("00c0ffee00c0ffee".to_string()),
        }
    );
}

#[test]
fn swap_conflict_is_a_reconcilable_outcome_not_an_error() {
    let stub = spawn_stub(|_req| {
        Response::Conflict(Conflict {
            expected: "0badc0de0badc0de".to_string(),
            actual: "00c0ffee00c0ffee".to_string(),
        })
    });
    let client = StructureClient::new(stub.addr.to_string());
    let outcome = within(Duration::from_secs(5), "swap conflict", move || {
        client
            .swap(
                DocSource::Path("p.json".to_string()),
                Some("0badc0de0badc0de".to_string()),
            )
            .expect("a conflict is a normal swap outcome, not a transport error")
    });
    assert_eq!(
        outcome,
        SwapOutcome::Conflict(Conflict {
            expected: "0badc0de0badc0de".to_string(),
            actual: "00c0ffee00c0ffee".to_string(),
        })
    );
}

#[test]
fn get_document_round_trips_the_doc_and_hash() {
    let document = serde_json::json!({ "format_version": 3, "instrument": "warm", "nodes": [] });
    let returned = document.clone();
    let stub = spawn_stub(move |_req| {
        Response::Document(DocumentSnapshot {
            document: returned.clone(),
            content_hash: "00c0ffee00c0ffee".to_string(),
        })
    });
    let client = StructureClient::new(stub.addr.to_string());
    let snapshot = within(Duration::from_secs(5), "get_document", move || {
        client.get_document().expect("get_document resolves")
    });
    assert_eq!(
        snapshot,
        DocumentSnapshot {
            document,
            content_hash: "00c0ffee00c0ffee".to_string(),
        }
    );
}

#[test]
fn get_diagnostics_round_trips_the_four_counters() {
    let report = DiagnosticsReport {
        output_xruns: 2,
        input_ring_underruns: 480,
        input_ring_overruns: 0,
        input_ring_producer_drops: 96,
    };
    let stub = spawn_stub(move |_req| Response::Diagnostics(report));
    let client = StructureClient::new(stub.addr.to_string());
    let got = within(Duration::from_secs(5), "get_diagnostics", move || {
        client.get_diagnostics().expect("get_diagnostics resolves")
    });
    assert_eq!(got, report);
}

#[test]
fn dead_port_fails_fast_with_start_reuben_play_guidance() {
    // An unreachable engine is a fail-fast with actionable guidance, never a hang or
    // a panic. The watchdog is generous — a refused connect returns immediately — so blowing it
    // means the client hung, which is itself the failure this test guards against.
    let addr = dead_addr();
    let err = within(Duration::from_secs(5), "dead-port connect", move || {
        StructureClient::new(addr)
            .ping()
            .expect_err("a dead port must fail, not succeed")
    });
    let message = err.to_string();
    assert!(
        message.contains("reuben play"),
        "the error must name the fix (`reuben play`): {message}"
    );
}

#[test]
fn wedged_server_read_times_out_instead_of_hanging() {
    // A server that accepts the connection but never answers must not wedge the sidecar: the read
    // timeout fires and the call returns an unreachable error well inside the watchdog window.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind wedged stub");
    let addr = listener.local_addr().expect("wedged addr");
    thread::spawn(move || {
        // Accept and hold the connections open forever, reading nothing, answering nothing.
        let mut held = Vec::new();
        for stream in listener.incoming() {
            match stream {
                Ok(s) => held.push(s),
                Err(_) => break,
            }
        }
    });
    // A short read timeout keeps the test fast; the watchdog is comfortably longer.
    let client = StructureClient::with_timeouts(
        addr.to_string(),
        Duration::from_millis(500),
        Duration::from_millis(300),
    );
    let err = within(Duration::from_secs(5), "wedged read", move || {
        client
            .ping()
            .expect_err("a wedged server must time out, not hang")
    });
    assert!(
        err.to_string().contains("reuben play"),
        "a read timeout is still an unreachable-engine failure: {err}"
    );
}

#[test]
fn ping_fails_fast_even_when_the_general_read_budget_is_generous() {
    // #374 tightening: a `ping`'s pong is immediate, so the liveness probe must not inherit the
    // generous read budget a real swap earns. Wedge a server, hand the client a deliberately huge
    // general read timeout, and assert `ping` still returns on its own tight budget — otherwise a
    // hung engine would stall `engine_status` and the probe-first `send` for the full read timeout.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind wedged stub");
    let addr = listener.local_addr().expect("wedged addr");
    thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            match stream {
                Ok(s) => held.push(s),
                Err(_) => break,
            }
        }
    });
    // General read timeout is 10s; the ping budget is capped far below it (DEFAULT_PING_READ_TIMEOUT).
    let client = StructureClient::with_timeouts(
        addr.to_string(),
        Duration::from_millis(500),
        Duration::from_secs(10),
    );
    // The 6s watchdog is comfortably above the ping budget but far below the 10s general timeout, so
    // a ping that wrongly inherited the general budget trips it; the elapsed assertion pins it sharp.
    let elapsed = within(Duration::from_secs(6), "tight ping", move || {
        let start = Instant::now();
        client
            .ping()
            .expect_err("a wedged server must time out, not hang");
        start.elapsed()
    });
    assert!(
        elapsed < Duration::from_secs(3),
        "ping must fail on its own tight budget, not the 10s general read timeout: took {elapsed:?}"
    );
}

#[test]
fn engine_link_pings_reachable_only_when_the_engine_answers() {
    // The reachability the engine tools consult: EngineLink.ping() succeeds against a
    // live ping-answering engine and fails (unreachable) against a dead port. This is the real seam
    // wired into `ReubenServer` — `engine_status` and the probe-first `send` read it, and the four
    // structure-channel tools act-then-map its unreachable case.
    let live = spawn_stub(|req| match req {
        Request::Ping => Response::Pong,
        _ => Response::Error {
            message: "no".to_string(),
        },
    });
    let link = EngineLink::new(live.addr.to_string(), default_osc_addr());
    assert!(
        within(Duration::from_secs(5), "link live", move || link
            .structure()
            .ping()
            .is_ok()),
        "a live ping-answering engine must read as reachable"
    );

    let dead = EngineLink::new(dead_addr(), default_osc_addr());
    let err = within(Duration::from_secs(5), "link dead", move || {
        dead.structure().ping()
    });
    assert!(
        err.is_err_and(|e| e.is_unreachable()),
        "a dead port must read as unreachable"
    );
}

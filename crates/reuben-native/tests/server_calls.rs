//! The engine serving a client's authoring, end to end over real loopback TCP against the native
//! entry point (headless: CI has no audio device): list, call, subscribe and open, and what each
//! leaves on disk.
//!
//! A [`RunningEngine`](reuben_native::engine::RunningEngine) is not `Send`, so the whole test runs
//! inside [`within`] and tears the engine down through [`timed_teardown`].

use std::path::Path;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use reuben_api::engine::{
    ChangedBy, Event, ListWhat, Request, Response, TcpTransport, Transport, DEFAULT_CONNECT_TIMEOUT,
};
use reuben_native::engine::{EngineConfig, Instrument};
use reuben_native::test_support::{start_headless, timed_teardown, within};

const DOC: &str = r#"{"format_version":3,"instrument":"first",
    "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
    "nodes":[{"type":"oscillator","address":"/osc"}]}"#;

const OTHER: &str = r#"{"format_version":3,"instrument":"second",
    "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
    "nodes":[{"type":"oscillator","address":"/osc"}]}"#;

const HANG_SECS: u64 = 30;
const REPLY: Duration = Duration::from_secs(10);

fn call(t: &TcpTransport, tool: &str, args: serde_json::Value) -> Response {
    ask(
        t,
        &Request::Call {
            tool: tool.to_string(),
            args,
        },
    )
}

fn ask(t: &TcpTransport, request: &Request) -> Response {
    let reply = t
        .round_trip(&request.to_ndjson(), REPLY)
        .expect("the engine answered");
    Response::from_ndjson(&reply).expect("a response")
}

fn output(response: Response) -> serde_json::Value {
    match response {
        Response::Called { output, .. } => output,
        other => panic!("expected a called answer, got {other:?}"),
    }
}

/// The events a subscription yields, read on a thread of its own so a missing one fails a
/// deadline rather than hanging.
fn follow(t: &TcpTransport) -> Receiver<Event> {
    let events = t.subscribe().expect("subscribed");
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for event in events.map_while(Result::ok) {
            if tx.send(event).is_err() {
                break;
            }
        }
    });
    rx
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).expect("dir");
    std::fs::write(path, body).expect("seed a file");
}

#[test]
fn a_client_lists_edits_and_opens_over_tcp() {
    let dir = std::env::temp_dir().join(format!("reuben_server_calls_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let library = dir.join("library");
    let open_dir = dir.join("open");
    let first = open_dir.join("first.json");
    let second = library.join("second.json");
    write(&first, DOC);
    write(&second, OTHER);
    let sample_in_library = library.join("samples").join("testvox.wav");
    write(&sample_in_library, "");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../instruments/samples/testvox.wav"),
        &sample_in_library,
    )
    .expect("copy a sample");
    let beside = open_dir.join("sounds").join("beside.wav");
    write(&beside, "");
    std::fs::copy(&sample_in_library, &beside).expect("copy a sample beside the document");
    std::fs::copy(&sample_in_library, dir.join("outside.wav")).expect("copy a sample outside");

    let config = EngineConfig {
        osc_in: Some("127.0.0.1:0".to_string()),
        structure: Some("127.0.0.1:0".to_string()),
        instrument_root: Some(library.clone()),
        ..EngineConfig::new(Instrument::Path(first.clone()))
    };
    let (first_s, second_s) = (first.display().to_string(), second.display().to_string());
    let sample_s = sample_in_library.display().to_string();

    within(HANG_SECS, move || {
        let engine = start_headless(config).expect("the engine starts");
        let addr = engine.structure_addr().expect("a structure socket");
        let t = TcpTransport::new(addr.to_string(), DEFAULT_CONNECT_TIMEOUT);

        match ask(
            &t,
            &Request::List {
                what: ListWhat::Instruments,
            },
        ) {
            Response::Listed { names } => {
                assert!(names.contains(&first_s), "{names:?}");
                assert!(names.contains(&second_s), "{names:?}");
            }
            other => panic!("expected a listing, got {other:?}"),
        }

        let described = call(
            &t,
            "describe_instrument",
            serde_json::json!({"source": first_s}),
        );
        assert!(
            matches!(described, Response::Called { .. }),
            "{described:?}"
        );

        let events = follow(&t);

        // A valid edit is written, announced once as the client's, and not echoed back as an
        // outside change by the server's own file watcher.
        let edit = output(call(
            &t,
            "set_instrument_name",
            serde_json::json!({"source": first_s, "name": "renamed"}),
        ));
        assert_eq!(edit["written"], true, "{edit}");
        assert!(std::fs::read_to_string(&first_s)
            .unwrap()
            .contains("renamed"));
        match events.recv_timeout(REPLY).expect("a changed event") {
            Event::Changed { hash, by } => {
                assert_eq!(by, ChangedBy::Client);
                assert_eq!(serde_json::json!(hash), edit["hash"]);
            }
            other => panic!("expected a changed event, got {other:?}"),
        }
        if let Ok(extra) = events.recv_timeout(Duration::from_millis(1000)) {
            panic!("the server's own write came back as {extra:?}");
        }

        // A breaking edit writes nothing.
        let before = std::fs::read(&first_s).unwrap();
        let broken = output(call(
            &t,
            "add_instrument_node",
            serde_json::json!({"source": first_s, "type": "no_such_operator", "address": "/x"}),
        ));
        assert_eq!(broken["written"], false, "{broken}");
        assert_eq!(std::fs::read(&first_s).unwrap(), before);

        // The sample names the server lists are the ones it accepts back.
        let samples = match ask(
            &t,
            &Request::List {
                what: ListWhat::Samples,
            },
        ) {
            Response::Listed { names } => names,
            other => panic!("expected a listing, got {other:?}"),
        };
        assert!(samples.contains(&sample_s), "{samples:?}");
        let added = output(call(
            &t,
            "add_instrument_resource",
            serde_json::json!({"source": first_s, "id": "listed", "resource_source": sample_s}),
        ));
        assert_eq!(added["written"], true, "{added}");

        // A resource relative to the document and inside its directory is written.
        let relative = output(call(
            &t,
            "add_instrument_resource",
            serde_json::json!({
                "source": first_s, "id": "beside", "resource_source": "sounds/beside.wav"
            }),
        ));
        assert_eq!(relative["written"], true, "{relative}");

        // One that climbs out of every offered directory is refused, and nothing is written.
        let before = std::fs::read(&first_s).unwrap();
        let escaped = call(
            &t,
            "add_instrument_resource",
            serde_json::json!({
                "source": first_s, "id": "escape", "resource_source": "../outside.wav"
            }),
        );
        assert!(matches!(escaped, Response::Refused { .. }), "{escaped:?}");
        assert_eq!(std::fs::read(&first_s).unwrap(), before);
        assert!(!before.windows(6).any(|w| w == b"escape"));

        // Opening another library instrument plays it and announces it.
        let report = ask(
            &t,
            &Request::Open {
                name: second_s.clone(),
                expect: None,
            },
        );
        assert!(matches!(report, Response::SwapReport(_)), "{report:?}");
        let opened = loop {
            match events.recv_timeout(REPLY).expect("an opened event") {
                Event::Opened { name, .. } => break name,
                _ => continue,
            }
        };
        assert_eq!(opened, second_s);

        timed_teardown(engine, false);
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// A fresh directory for `case`, removed first: process-unique, so concurrent checkouts do not
/// share it.
fn scratch(case: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("reuben_{case}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Serve `open` with `library` as the instrument root, run `f` against a client of it, and tear
/// the engine down.
fn serving(open: &Path, library: &Path, f: impl FnOnce(&TcpTransport) + Send + 'static) {
    let config = EngineConfig {
        osc_in: Some("127.0.0.1:0".to_string()),
        structure: Some("127.0.0.1:0".to_string()),
        instrument_root: Some(library.to_path_buf()),
        ..EngineConfig::new(Instrument::Path(open.to_path_buf()))
    };
    within(HANG_SECS, move || {
        let engine = start_headless(config).expect("the engine starts");
        let addr = engine.structure_addr().expect("a structure socket");
        f(&TcpTransport::new(
            addr.to_string(),
            DEFAULT_CONNECT_TIMEOUT,
        ));
        timed_teardown(engine, false);
    });
}

#[test]
fn a_create_named_like_a_library_file_leaves_the_library_file_alone() {
    let dir = scratch("server_calls_create_library");
    let library = dir.join("library");
    let open = dir.join("open").join("first.json");
    write(&open, DOC);
    let in_library = library.join("pad.json");
    write(&in_library, OTHER);

    serving(&open, &library, |t| {
        let created = call(
            t,
            "new_instrument",
            serde_json::json!({"source": "pad.json", "name": "fresh"}),
        );
        assert!(matches!(created, Response::Refused { .. }), "{created:?}");
    });
    assert_eq!(std::fs::read_to_string(&in_library).unwrap(), OTHER);
    assert!(!dir.join("open").join("pad.json").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The answer to reading `../lib/a.json` from an instrument open in `proj/open/`, with the library
/// root at `lib/` holding `a.json`, and `proj/lib/a.json` outside every offered directory present
/// or not.
fn read_climbing_out(case: &str, outside_exists: bool) -> Response {
    let dir = scratch(case);
    let library = dir.join("lib");
    let open = dir.join("proj").join("open").join("first.json");
    write(&open, DOC);
    write(&library.join("a.json"), OTHER);
    if outside_exists {
        write(&dir.join("proj").join("lib").join("a.json"), OTHER);
    }
    let (tx, rx) = channel();
    serving(&open, &library, move |t| {
        let _ = tx.send(call(
            t,
            "describe_instrument",
            serde_json::json!({"source": "../lib/a.json"}),
        ));
    });
    let _ = std::fs::remove_dir_all(&dir);
    rx.recv().expect("an answer")
}

#[test]
fn a_name_that_climbs_out_of_the_open_directory_answers_the_same_whatever_is_there() {
    let absent = read_climbing_out("server_calls_climb_absent", false);
    let present = read_climbing_out("server_calls_climb_present", true);
    assert_eq!(absent, present);
    assert!(matches!(present, Response::Refused { .. }), "{present:?}");
}

#[test]
fn a_read_that_fails_names_the_file_as_the_client_spelled_it() {
    let dir = scratch("server_calls_read_spelling");
    let library = dir.join("lib");
    let open = dir.join("open").join("first.json");
    write(&open, DOC);
    std::fs::create_dir_all(&library).unwrap();
    let server_path = dir.display().to_string();
    serving(&open, &library, move |t| {
        match call(
            t,
            "describe_instrument",
            serde_json::json!({"source": "missing.json"}),
        ) {
            Response::Refused { message } => {
                assert!(message.contains("missing.json"), "{message}");
                assert!(!message.contains(&server_path), "{message}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
}

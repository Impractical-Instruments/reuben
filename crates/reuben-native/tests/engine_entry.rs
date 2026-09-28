//! The entry point's failure modes and teardown, headless (CI has no audio device): what is fatal,
//! what is reported and survived, and that every teardown finishes — promptly — even when the
//! render callback outlives it.
//!
//! A [`RunningEngine`](reuben_native::engine::RunningEngine) is not `Send`, so each test that starts
//! one runs whole inside [`within`], and tears it down through [`timed_teardown`].

use std::net::{TcpListener, UdpSocket};

use reuben_api::engine::get_engine_status;
use reuben_api::render::{Arg, Message};
use reuben_native::engine::{EngineConfig, Instrument, StartError};
use reuben_native::test_support::{
    start_headless, start_headless_holding_outbound, timed_teardown, within,
};

const OSC_DOC: &str = r#"{"format_version":3,"instrument":"t",
    "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
    "nodes":[{"type":"oscillator","address":"/osc"}]}"#;

/// The hang deadline. Generous next to the shutdown bound, so a hang and a slow teardown fail as
/// different messages.
const HANG_SECS: u64 = 10;

fn seed(test: &str, body: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "reuben_engine_entry_{test}_{}.json",
        std::process::id()
    ));
    std::fs::write(&path, body).expect("seed a document");
    path
}

fn config(instrument: Instrument) -> EngineConfig {
    EngineConfig {
        osc_in: Some("127.0.0.1:0".to_string()),
        structure: Some("127.0.0.1:0".to_string()),
        ..EngineConfig::new(instrument)
    }
}

#[test]
fn a_missing_instrument_file_is_a_read_error() {
    let missing = std::env::temp_dir().join("reuben_engine_entry_does_not_exist.json");
    match start_headless(config(Instrument::Path(missing.clone()))) {
        Err(StartError::ReadInstrument { path, .. }) => assert_eq!(path, missing),
        Err(other) => panic!("expected a read error, got {other}"),
        Ok(_) => panic!("a missing instrument must not start"),
    }
}

#[test]
fn an_unloadable_instrument_is_an_instrument_error() {
    let path = seed(
        "unloadable",
        r#"{"format_version":3,"instrument":"t","nodes":[{"type":"no_such_operator","address":"/x"}]}"#,
    );
    match start_headless(config(Instrument::Path(path.clone()))) {
        Err(StartError::Instrument(_)) => {}
        Err(other) => panic!("expected an instrument error, got {other}"),
        Ok(_) => panic!("an unloadable instrument must not start"),
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_taken_osc_port_is_fatal() {
    let path = seed("osc_taken", OSC_DOC);
    let taken = UdpSocket::bind("127.0.0.1:0").expect("hold a port");
    let cfg = EngineConfig {
        osc_in: Some(taken.local_addr().unwrap().to_string()),
        ..config(Instrument::Path(path.clone()))
    };
    assert!(matches!(start_headless(cfg), Err(StartError::OscIn { .. })));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_non_loopback_structure_address_is_refused_before_anything_binds() {
    let path = seed("non_loopback", OSC_DOC);
    let cfg = EngineConfig {
        structure: Some("0.0.0.0:0".to_string()),
        ..config(Instrument::Path(path.clone()))
    };
    assert!(matches!(
        start_headless(cfg),
        Err(StartError::StructureNotLoopback(_))
    ));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_taken_structure_port_is_survived_and_the_in_process_channel_still_answers() {
    let path = seed("structure_taken", OSC_DOC);
    let taken = TcpListener::bind("127.0.0.1:0").expect("hold a port");
    let cfg = EngineConfig {
        structure: Some(taken.local_addr().unwrap().to_string()),
        ..config(Instrument::Path(path.clone()))
    };
    within(HANG_SECS, move || {
        let engine = start_headless(cfg).expect("a taken structure port is not fatal");
        assert!(engine.structure_addr().is_none());
        assert!(engine.structure_error().is_some());
        assert!(
            get_engine_status(&engine.channel(), "test")
                .output
                .reachable
        );
        timed_teardown(engine, false);
    });
    drop(taken);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn no_sockets_at_all_is_a_valid_embedding() {
    let path = seed("no_sockets", OSC_DOC);
    let cfg = EngineConfig {
        osc_in: None,
        structure: None,
        ..EngineConfig::new(Instrument::Path(path.clone()))
    };
    within(HANG_SECS, move || {
        let engine = start_headless(cfg).expect("starts with no sockets");
        assert!(engine.osc_in_addr().is_none() && engine.structure_addr().is_none());
        assert!(engine.structure_error().is_none());
        assert!(
            get_engine_status(&engine.channel(), "test")
                .output
                .reachable
        );
        timed_teardown(engine, true);
    });
    let _ = std::fs::remove_file(&path);
}

/// The render callback's outbound sender outlives the teardown here, as it does when cpal never
/// drops a callback, so the OSC-out thread cannot end on its channel disconnecting: the engine has
/// to stop it. A failed `send` afterwards is the proof the thread (and its receiver) is gone.
#[test]
fn shutdown_stops_osc_out_even_when_the_render_callback_outlives_it() {
    let path = seed("osc_out_held", OSC_DOC);
    let sink = UdpSocket::bind("127.0.0.1:0").expect("an OSC-out sink");
    let target = sink.local_addr().unwrap().to_string();
    let cfg = EngineConfig {
        osc_out: Some(target.clone()),
        ..config(Instrument::Path(path.clone()))
    };
    let held = within(HANG_SECS, move || {
        let (engine, held) = start_headless_holding_outbound(cfg).expect("starts with OSC-out");
        assert_eq!(engine.osc_out_target(), Some(target.as_str()));
        timed_teardown(engine, false);
        held.expect("an OSC-out target gives the callback a sender")
    });
    assert!(
        held.send(Message::new("/after", Arg::F32(1.0), 0)).is_err(),
        "the OSC-out thread is still running after shutdown"
    );
    let _ = std::fs::remove_file(&path);
}

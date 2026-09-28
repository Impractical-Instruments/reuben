//! The entry point's failure modes, headless (CI has no audio device): what is fatal, what is
//! reported and survived, and that a failed start leaves nothing bound behind it.

use std::net::{TcpListener, UdpSocket};

use reuben_api::engine::get_engine_status;
use reuben_native::engine::{EngineConfig, Instrument, StartError};
use reuben_native::test_support::start_headless;

const OSC_DOC: &str = r#"{"format_version":3,"instrument":"t",
    "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
    "nodes":[{"type":"oscillator","address":"/osc"}]}"#;

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
fn an_unloadable_instrument_is_an_instrument_error_and_frees_the_osc_port() {
    let path = seed(
        "unloadable",
        r#"{"format_version":3,"instrument":"t","nodes":[{"type":"no_such_operator","address":"/x"}]}"#,
    );
    // Bind-then-release to learn a free port, so the failed start's own bind can be checked after.
    let osc_in = UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let cfg = EngineConfig {
        osc_in: Some(osc_in.to_string()),
        ..config(Instrument::Path(path.clone()))
    };
    match start_headless(cfg) {
        Err(StartError::Instrument(_)) => {}
        Err(other) => panic!("expected an instrument error, got {other}"),
        Ok(_) => panic!("an unloadable instrument must not start"),
    }
    UdpSocket::bind(osc_in).expect("the failed start released the OSC-in port");
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
fn a_taken_structure_port_is_survived_and_the_in_process_channel_still_answers() {
    let path = seed("structure_taken", OSC_DOC);
    let taken = TcpListener::bind("127.0.0.1:0").expect("hold a port");
    let cfg = EngineConfig {
        structure: Some(taken.local_addr().unwrap().to_string()),
        ..config(Instrument::Path(path.clone()))
    };
    let engine = start_headless(cfg).expect("a taken structure port is not fatal");
    assert!(engine.structure_addr().is_none());
    assert!(engine.structure_error().is_some());
    assert!(
        get_engine_status(&engine.channel(), "test")
            .output
            .reachable
    );
    engine.shutdown();
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
    let engine = start_headless(cfg).expect("starts with no sockets");
    assert!(engine.osc_in_addr().is_none() && engine.structure_addr().is_none());
    assert!(engine.structure_error().is_none());
    assert!(
        get_engine_status(&engine.channel(), "test")
            .output
            .reachable
    );
    drop(engine);
    let _ = std::fs::remove_file(&path);
}

//! The remote attach, on one machine: a client reaches the engine through this machine's own
//! non-loopback address, the way one on another machine would over a LAN or Tailscale, holds no
//! file, and does everything an attached editor does — list, open, read a nested instrument, edit,
//! hear the meters, and follow a change — only with the engine's token.
//!
//! Skipped, saying so, on a machine with no route off loopback.

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::Path;
use std::sync::mpsc::channel;
use std::time::Duration;

use reuben_api::engine::{
    Channel, Event, ListWhat, TcpTransport, Token, DEFAULT_CONNECT_TIMEOUT, REFUSED_TOKEN,
    TOKEN_REQUIRED,
};
use reuben_api::tools::names;
use reuben_native::engine::{EngineConfig, Instrument};
use reuben_native::test_support::{start_headless, timed_teardown, within};

const LEAF: &str = r#"{"format_version":3,"instrument":"leaf",
    "interface":{"outputs":{"audio":{"from":"/osc.audio"}}},
    "nodes":[{"type":"oscillator","address":"/osc"}]}"#;

const TOP: &str = r#"{"format_version":3,"instrument":"top",
    "interface":{"outputs":{"out":{"from":"/voice.audio"}}},
    "resources":{"leaf":"leaf.json"},
    "nodes":[{"type":"subpatch","address":"/voice","patch":"leaf"}]}"#;

const HANG_SECS: u64 = 30;

/// This machine's address on the interface its default route leaves by. Connecting a UDP socket
/// sends nothing; it only picks the route.
fn off_loopback() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

fn channel_to(addr: SocketAddr, token: Option<Token>) -> Channel {
    Channel::new(TcpTransport::new(addr.to_string(), DEFAULT_CONNECT_TIMEOUT).with_token(token))
}

fn refusal(channel: &Channel) -> String {
    match channel.ping() {
        Err(reuben_api::engine::ChannelError::Channel(message)) => message,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_client_on_another_interface_does_everything_with_the_token() {
    let Some(ip) = off_loopback() else {
        eprintln!("skipped: no non-loopback interface");
        return;
    };
    let dir = std::env::temp_dir().join(format!("reuben_structure_remote_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let top = dir.join("top.json");
    std::fs::write(&top, TOP).expect("seed the top");
    std::fs::write(dir.join("leaf.json"), LEAF).expect("seed the leaf");
    let top_s = top.display().to_string();

    within(HANG_SECS, move || {
        let token = Token::new("s3cret").expect("a token");
        let engine = start_headless(EngineConfig {
            osc_in: None,
            structure: Some("0.0.0.0:0".to_string()),
            token: Some(token.clone()),
            follow_files: false,
            ..EngineConfig::new(Instrument::Silent)
        })
        .expect("the engine starts");
        engine.offer_dir(&dir);
        let addr = SocketAddr::new(ip, engine.structure_addr().expect("bound").port());

        assert_eq!(refusal(&channel_to(addr, None)), TOKEN_REQUIRED);
        let guess = Token::new("guess").expect("a token");
        assert_eq!(refusal(&channel_to(addr, Some(guess))), REFUSED_TOKEN);

        let client = channel_to(addr, Some(token.clone()));
        client.ping().expect("the token is admitted");

        let listed = client.list(ListWhat::Instruments).expect("a listing");
        assert!(listed.contains(&top_s), "{listed:?}");

        let opened = client.open(top_s.clone(), None).expect("opened");
        assert!(
            matches!(opened, reuben_api::engine::SwapOutcome::Installed(ref r) if r.report.ok),
            "{opened:?}"
        );
        let before = client.get_document().expect("what plays").content_hash;

        let described = client
            .call(
                names::DESCRIBE_INSTRUMENT,
                serde_json::json!({"source": top_s}),
            )
            .expect("described");
        assert!(format!("{described:?}").contains("/voice"), "{described:?}");

        // A second client follows; the edit the first makes reaches it.
        let follower =
            TcpTransport::new(addr.to_string(), DEFAULT_CONNECT_TIMEOUT).with_token(Some(token));
        let events = follower.subscribe().expect("subscribed");
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for event in events.map_while(Result::ok) {
                if tx.send(event).is_err() {
                    break;
                }
            }
        });

        client
            .call(
                names::SET_INSTRUMENT_NAME,
                serde_json::json!({"source": top_s, "name": "renamed"}),
            )
            .expect("edited");
        let after = client.get_document().expect("what plays").content_hash;
        assert_ne!(after, before, "the edit is what plays");
        match rx.recv_timeout(Duration::from_secs(10)).expect("an event") {
            Event::Changed { hash, .. } => assert_eq!(hash, after),
            other => panic!("expected a change, got {other:?}"),
        }

        client.get_meters().expect("the meters answer");

        timed_teardown(engine, false);
        let _ = std::fs::remove_dir_all(Path::new(&dir));
    });
}

#[test]
fn a_peer_elsewhere_that_never_presents_the_token_is_closed() {
    use std::io::{BufRead, BufReader, Write};

    let Some(ip) = off_loopback() else {
        eprintln!("skipped: no non-loopback interface");
        return;
    };
    within(HANG_SECS, move || {
        let engine = start_headless(EngineConfig {
            osc_in: None,
            structure: Some("0.0.0.0:0".to_string()),
            token: Some(Token::new("s3cret").expect("a token")),
            follow_files: false,
            ..EngineConfig::new(Instrument::Silent)
        })
        .expect("the engine starts");
        let addr = SocketAddr::new(ip, engine.structure_addr().expect("bound").port());

        // Silent, and blank lines: neither is a first line, so neither may hold the connection.
        for blank in ["", "\n\n  \n"] {
            let stream = std::net::TcpStream::connect(addr).expect("connect");
            (&stream).write_all(blank.as_bytes()).expect("write");
            stream
                .set_read_timeout(Some(Duration::from_secs(15)))
                .expect("a read timeout");
            let mut reply = String::new();
            BufReader::new(&stream)
                .read_line(&mut reply)
                .expect("the engine answers before closing");
            assert!(reply.contains(TOKEN_REQUIRED), "{blank:?}: {reply:?}");
            let mut rest = String::new();
            assert_eq!(
                BufReader::new(&stream)
                    .read_line(&mut rest)
                    .expect("closed"),
                0,
                "{blank:?}: the connection is closed"
            );
        }
        timed_teardown(engine, false);
    });
}

//! `reuben play --headless` as a client starts it: the built binary, no audio device, the
//! structure channel and OSC-in on chosen loopback ports.

use std::io::{BufRead, BufReader};
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use reuben_api::authoring;
use reuben_api::engine::{Channel, TcpTransport, DEFAULT_CONNECT_TIMEOUT};
use reuben_api::FsResolver;

const DOC: &str = r#"{"format_version":3,"instrument":"headless",
    "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
    "nodes":[{"type":"oscillator","address":"/osc"}]}"#;

/// How long the binary may take to bind and announce its structure channel.
const STARTUP: Duration = Duration::from_secs(30);

/// Kills the child and removes the seeded directory however the test ends, so a failed assertion
/// leaves no server holding ports and nothing in the temp dir.
struct Cleanup {
    child: Option<Child>,
    dir: PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn free_tcp_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free TCP port")
        .port()
}

fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .and_then(|s| s.local_addr())
        .expect("a free UDP port")
        .port()
}

fn hash(path: &Path) -> String {
    let source = path.display().to_string();
    let json = std::fs::read_to_string(path).expect("read the seeded document");
    authoring::hash_instrument(&json, &FsResolver::for_document(&source)).expect("a hash")
}

#[test]
fn play_headless_serves_the_structure_channel_on_the_chosen_port() {
    let dir = std::env::temp_dir().join(format!("reuben_play_headless_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cleanup = Cleanup {
        child: None,
        dir: dir.clone(),
    };
    std::fs::create_dir_all(&dir).expect("dir");
    let doc = dir.join("headless.json");
    std::fs::write(&doc, DOC).expect("seed the document");

    let structure = format!("127.0.0.1:{}", free_tcp_port());
    let osc_in = format!("127.0.0.1:{}", free_udp_port());
    let child = cleanup.child.insert(
        Command::new(env!("CARGO_BIN_EXE_reuben"))
            .args(["play", "--headless", "--structure-addr", &structure])
            .args(["--osc-in", &osc_in])
            .arg(&doc)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn reuben play"),
    );

    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let announced = loop {
        let line = rx
            .recv_timeout(STARTUP)
            .expect("reuben play announced its structure channel");
        if let Some(addr) = line.strip_prefix("structure channel on ") {
            break addr.to_string();
        }
    };
    assert_eq!(announced, structure);

    let channel = Channel::new(TcpTransport::new(announced, DEFAULT_CONNECT_TIMEOUT));
    channel.ping().expect("ping");
    let snapshot = channel.get_document().expect("get_document");
    assert_eq!(snapshot.content_hash, hash(&doc));

    drop(cleanup);
}

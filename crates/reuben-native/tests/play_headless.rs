//! `reuben play --headless` as a client starts it: the built binary, no audio device, the
//! structure channel and OSC-in on chosen loopback ports.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

mod common;

use common::{announced_structure, free_tcp_port, free_udp_port};

use reuben_api::authoring;
use reuben_api::engine::{Channel, ListWhat, TcpTransport, DEFAULT_CONNECT_TIMEOUT};
use reuben_api::FsResolver;

const DOC: &str = r#"{"format_version":3,"instrument":"headless",
    "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
    "nodes":[{"type":"oscillator","address":"/osc"}]}"#;

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

fn hash(path: &Path) -> String {
    let source = path.display().to_string();
    let json = std::fs::read_to_string(path).expect("read the seeded document");
    authoring::hash_instrument(&json, &FsResolver::for_document(&source)).expect("a hash")
}

#[test]
fn a_bare_play_offers_the_instruments_directory_it_starts_in() {
    let dir = std::env::temp_dir().join(format!("reuben_play_bare_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cleanup = Cleanup {
        child: None,
        dir: dir.clone(),
    };
    std::fs::create_dir_all(dir.join("instruments")).expect("dir");
    std::fs::write(dir.join("instruments").join("headless.json"), DOC).expect("seed");

    let structure = format!("127.0.0.1:{}", free_tcp_port());
    let osc_in = format!("127.0.0.1:{}", free_udp_port());
    let child = cleanup.child.insert(
        Command::new(env!("CARGO_BIN_EXE_reuben"))
            .args(["play", "--headless", "--structure-addr", &structure])
            .args(["--osc-in", &osc_in])
            .env_remove(reuben_native::cli::INSTRUMENT_ROOT_ENV)
            .current_dir(&dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn reuben play"),
    );
    let announced = announced_structure(child);

    let channel = Channel::new(TcpTransport::new(announced, DEFAULT_CONNECT_TIMEOUT));
    let names = channel.list(ListWhat::Instruments).expect("list");
    assert!(
        names
            .iter()
            .any(|name| Path::new(name).ends_with("instruments/headless.json")),
        "{names:?}"
    );

    drop(cleanup);
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

    let announced = announced_structure(child);
    assert_eq!(announced, structure);

    let channel = Channel::new(TcpTransport::new(announced, DEFAULT_CONNECT_TIMEOUT));
    channel.ping().expect("ping");
    let snapshot = channel.get_document().expect("get_document");
    assert_eq!(snapshot.content_hash, hash(&doc));

    drop(cleanup);
}

//! Helpers the binary tests share: a seeded temp directory, a headless `reuben play` on free
//! loopback ports, and the `reuben` CLI run in a chosen directory.
//!
//! Every process started here has the structure-address and library-root environment variables
//! removed, so a developer's own settings cannot change a result.

// Each test crate that includes this module uses a different part of it.
#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use reuben_api::engine::{STRUCTURE_ADDR_ENV, STRUCTURE_TOKEN_ENV};
use reuben_native::cli::INSTRUMENT_ROOT_ENV;

/// How long the binary may take to bind and announce its structure channel.
const STARTUP: Duration = Duration::from_secs(30);

/// A minimal valid instrument named `name`.
pub fn doc(name: &str) -> String {
    format!(
        r#"{{"format_version":3,"instrument":"{name}",
        "interface":{{"outputs":{{"out":{{"from":"/osc.audio"}}}}}},
        "nodes":[{{"type":"oscillator","address":"/osc"}}]}}"#
    )
}

/// A fresh directory under the temp dir, removed when dropped.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(case: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("reuben_{case}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the test dir");
        TempDir(dir)
    }

    /// Write [`doc`]`(instrument)` at `name` in this directory.
    pub fn seed(&self, name: &str, instrument: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, doc(instrument)).expect("seed the document");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn free_tcp_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free TCP port")
        .port()
}

pub fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .and_then(|s| s.local_addr())
        .expect("a free UDP port")
        .port()
}

/// Collect `child`'s stdout lines on a thread and wait for its structure channel's announcement.
pub fn announced_structure(child: &mut Child) -> String {
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    loop {
        let line = rx
            .recv_timeout(STARTUP)
            .expect("reuben play announced its structure channel");
        if let Some(addr) = line.strip_prefix("structure channel on ") {
            return addr.to_string();
        }
    }
}

/// A headless `reuben play`, killed when dropped.
pub struct Play {
    child: Child,
    pub addr: String,
}

impl Drop for Play {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `reuben play --headless` in `cwd` with `args` after it, on free loopback ports.
pub fn spawn_play(cwd: &Path, args: &[&str]) -> Play {
    spawn_play_on(cwd, "127.0.0.1", args)
}

/// [`spawn_play`] with the structure channel on a free port of `ip`.
pub fn spawn_play_on(cwd: &Path, ip: &str, args: &[&str]) -> Play {
    let structure = format!("{ip}:{}", free_tcp_port());
    let osc_in = format!("127.0.0.1:{}", free_udp_port());
    let mut child = Command::new(env!("CARGO_BIN_EXE_reuben"))
        .args(["play", "--headless", "--structure-addr", &structure])
        .args(["--osc-in", &osc_in])
        .args(args)
        .env_remove(INSTRUMENT_ROOT_ENV)
        .env_remove(STRUCTURE_ADDR_ENV)
        .env_remove(STRUCTURE_TOKEN_ENV)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn reuben play");
    let addr = announced_structure(&mut child);
    Play { child, addr }
}

/// The `reuben` CLI with `args`, run in `cwd`.
pub fn reuben(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_reuben"))
        .args(args)
        .env_remove(INSTRUMENT_ROOT_ENV)
        .env_remove(STRUCTURE_ADDR_ENV)
        .env_remove(STRUCTURE_TOKEN_ENV)
        .current_dir(cwd)
        .output()
        .expect("the reuben binary runs")
}

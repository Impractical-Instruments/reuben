//! Integration: `reuben send` and `reuben live`, the text-editor clients, run as the binary against
//! a real engine-channel server on an ephemeral loopback port.
//!
//! The server is everything `reuben play` wires up but the audio device: a Coordinator, the native
//! host, the control ring, and [`FakeCallback`] draining that ring as the render callback does.
//! No built-in Operator compiles its text yet, so the installed Plan's registry adds the test-only
//! `steps` Operator, which compiles whitespace-separated whole numbers and refuses anything else
//! with the byte span at fault.

#[path = "../../reuben-document/tests/prepared/mod.rs"]
mod prepared;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reuben_api::engine::{ChannelState, Coordinator, ENGINE_ADDR_ENV, ENGINE_TOKEN_ENV};
use reuben_api::render::{Arg, AudioConfig};
use reuben_native::control::ring_pair;
use reuben_native::diagnostics::Diagnostics;
use reuben_native::engine_channel::{ChannelServer, HeadlessRenderConfig, NativeHost};
use reuben_native::test_support::{no_store, served, FakeCallback};

/// Two `steps` nodes, each input unfed, so a `send` to `/a/steps` or `/b/steps` is what it plays.
const RIG: &str = r#"{ "format_version": 3, "instrument": "live",
  "nodes": [
    { "type": "steps", "address": "/a", "inputs": { "steps": "1" } },
    { "type": "steps", "address": "/b", "inputs": { "steps": "2" } } ] }"#;

/// How long a test waits for something that should happen at once.
const PATIENCE: Duration = Duration::from_secs(10);

/// A running server, and the callback whose log says what reached the render thread.
struct Server {
    callback: FakeCallback,
    addr: String,
    _server: ChannelServer,
}

fn serve() -> Server {
    let (coordinator, side, _) = Coordinator::install_initial(
        RIG,
        prepared::registry(),
        Box::new(reuben_document::MemoryResolver::new()),
        AudioConfig::new(48_000.0, 128),
    )
    .expect("the rig installs");
    let (control_tx, control_rx) = ring_pair(Diagnostics::new());
    let host = NativeHost::new(Diagnostics::new(), control_tx).with_render_config(Arc::new(
        HeadlessRenderConfig {
            opened_input_channels: 0,
        },
    ));
    let state = ChannelState::new(coordinator, Arc::new(host), served(no_store()));
    let server = ChannelServer::bind("127.0.0.1:0", state).expect("bind the engine channel");
    Server {
        callback: FakeCallback::spawn(side, control_rx),
        addr: server.local_addr().to_string(),
        _server: server,
    }
}

impl Server {
    /// Every message the render thread was handed so far, as `(address, sum)`, waiting for `n`.
    fn queued(&self, n: usize) -> Vec<(String, u32)> {
        self.callback
            .await_queued(n)
            .into_iter()
            .map(|m| {
                let [Arg::Prepared(p)] = m.args.as_slice() else {
                    panic!(
                        "{} arrived as {:?}, not one compiled value",
                        m.address, m.args
                    );
                };
                let steps = p
                    .payload::<prepared::Compiled>()
                    .expect("steps compiled it");
                (m.address, steps.0.iter().sum())
            })
            .collect()
    }
}

/// An address nothing listens on.
fn dead_addr() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.local_addr().expect("addr").to_string()
}

/// The `reuben` binary, its server environment cleared so only the flags pick a server.
fn reuben() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_reuben"));
    cmd.env_remove(ENGINE_ADDR_ENV).env_remove(ENGINE_TOKEN_ENV);
    cmd
}

/// `reuben send --engine-addr <addr> <args…>`, with `stdin` piped in.
fn send(addr: &str, args: &[&str], stdin: &str) -> Output {
    let mut child = reuben()
        .args(["send", "--engine-addr", addr])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run reuben send");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("reuben send exits")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_bad_pattern_through_send_exits_non_zero_with_the_span_and_nothing_changes() {
    let server = serve();
    let refused = send(&server.addr, &["/a/steps", "1 x 3"], "");
    assert!(!refused.status.success(), "a refusal exits non-zero");
    assert_eq!(
        stderr(&refused),
        "<arg>:1:3: error: expected a whole number\n1 x 3\n  ^\n"
    );

    let piped = send(&server.addr, &["/a/steps"], "4 5\n6 z\n");
    assert!(!piped.status.success());
    assert!(
        stderr(&piped).starts_with("<stdin>:1:3: error: expected a whole number\n"),
        "{}",
        stderr(&piped)
    );

    let queued = send(&server.addr, &["/a/steps", "4 5"], "");
    assert!(queued.status.success(), "{}", stderr(&queued));
    assert_eq!(
        server.queued(1),
        vec![("/a/steps".to_string(), 9)],
        "only the send that compiled reached the render thread"
    );
}

#[test]
fn send_reads_the_pattern_from_stdin_when_it_is_left_out() {
    let server = serve();
    let output = send(&server.addr, &["/b/steps"], "7 8\n");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(server.queued(1), vec![("/b/steps".to_string(), 15)]);
}

#[test]
fn both_fail_fast_with_a_clear_message_when_no_reuben_play_is_reachable() {
    let dead = dead_addr();
    let dir = Dir::new("unreachable");
    let set = dir.0.join("a.set");
    std::fs::write(&set, "/a/steps 1\n").expect("write the set");
    for args in [
        vec!["send", "--engine-addr", &dead, "/a/steps", "1"],
        vec!["live", "--engine-addr", &dead, set.to_str().expect("utf-8")],
    ] {
        let started = Instant::now();
        let output = reuben()
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .expect("run reuben");
        assert!(!output.status.success(), "{args:?} exits non-zero");
        let shown = stderr(&output);
        assert!(shown.contains("reuben play"), "{args:?}: {shown}");
        assert!(shown.contains(&dead), "{args:?}: {shown}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{args:?} took {:?}",
            started.elapsed()
        );
    }

    let from_env = reuben()
        .env(ENGINE_ADDR_ENV, &dead)
        .args(["send", "/a/steps", "1"])
        .stdin(Stdio::null())
        .output()
        .expect("run reuben send");
    assert!(!from_env.status.success());
    assert!(stderr(&from_env).contains(&dead), "{}", stderr(&from_env));
}

/// A fresh directory under the temp dir, removed when dropped.
struct Dir(PathBuf);

impl Dir {
    fn new(case: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("reuben_live_{case}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the test dir");
        Dir(dir)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Save `text` at `path` as many editors do: write a temporary file beside it, then rename it over.
fn save(path: &Path, text: &str) {
    let temp = path.with_extension("swp");
    std::fs::write(&temp, text).expect("write the temporary file");
    std::fs::rename(&temp, path).expect("rename it over the set");
}

/// A running `reuben live`, killed on drop.
struct Live {
    child: Child,
    lines: Receiver<String>,
}

impl Live {
    fn start(addr: &str, set: &Path) -> Self {
        let mut child = reuben()
            .args(["live", "--engine-addr", addr])
            .arg(set)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("run reuben live");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        Live { child, lines }
    }

    /// The lines of one save's batch, from its `sending` line through its `done` line.
    fn batch(&self) -> Vec<String> {
        let deadline = Instant::now() + PATIENCE;
        let mut batch = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("no `done` line; so far: {batch:#?}"));
            let done = line.starts_with("reuben live: done");
            batch.push(line);
            if done {
                return batch;
            }
        }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn live_sends_only_changed_lines_skips_comments_and_keeps_the_old_pattern_when_a_line_fails() {
    let server = serve();
    let dir = Dir::new("changed");
    let set = dir.0.join("drums.set");
    std::fs::write(&set, "# two patterns\n/a/steps  1 2\n\n/b/steps 3\n").expect("write the set");
    let shown = set.display().to_string();
    let live = Live::start(&server.addr, &set);

    assert_eq!(
        live.batch(),
        vec![
            format!("reuben live: sending {shown}"),
            "reuben live: queued /a/steps (line 2)".to_string(),
            "reuben live: queued /b/steps (line 4)".to_string(),
            "reuben live: done (2 queued, 0 errors)".to_string(),
        ]
    );
    assert_eq!(
        server.queued(2),
        vec![("/a/steps".to_string(), 3), ("/b/steps".to_string(), 3)]
    );

    save(&set, "# two patterns\n/a/steps  1 2 3\n\n/b/steps 3 x\n");
    assert_eq!(
        live.batch(),
        vec![
            format!("reuben live: sending {shown}"),
            "reuben live: queued /a/steps (line 2)".to_string(),
            format!("{shown}:4:12: error: expected a whole number"),
            "3 x".to_string(),
            "  ^".to_string(),
            "reuben live: done (1 queued, 1 error)".to_string(),
        ]
    );
    // The failed line reached nothing, so `/b` still plays the 3 it was last sent.
    assert_eq!(
        server.queued(4),
        vec![
            ("/a/steps".to_string(), 3),
            ("/b/steps".to_string(), 3),
            ("/a/steps".to_string(), 6),
        ]
    );

    save(&set, "# two patterns\n/a/steps  1 2 3\n\n/b/steps 3 4\n");
    assert_eq!(
        live.batch(),
        vec![
            format!("reuben live: sending {shown}"),
            "reuben live: fixed, queued /b/steps (line 4)".to_string(),
            "reuben live: done (1 queued, 0 errors)".to_string(),
        ]
    );
    assert_eq!(
        server.queued(4).last(),
        Some(&("/b/steps".to_string(), 7)),
        "only the fixed line was sent"
    );
    assert_eq!(server.queued(5).len(), 4, "the unchanged `/a` line was not");
}

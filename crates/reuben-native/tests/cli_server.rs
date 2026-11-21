//! The `reuben` CLI as a client: which server answers, and in whose terms.

mod common;

use std::process::Output;

use common::{reuben, spawn_play, TempDir};
use reuben_api::engine::{Channel, TcpTransport, DEFAULT_CONNECT_TIMEOUT};

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn channel(addr: &str) -> Channel {
    Channel::new(TcpTransport::new(addr, DEFAULT_CONNECT_TIMEOUT))
}

#[test]
fn describe_reaches_the_running_server() {
    let a = TempDir::new("cli_reach_a");
    let b = TempDir::new("cli_reach_b");
    let doc = a.seed("doc.json", "reached");
    let root = a.0.display().to_string();
    let play = spawn_play(&a.0, &["--instrument-root", &root]);
    let source = doc.display().to_string();

    let running = reuben(&b.0, &["describe", &source, "--structure-addr", &play.addr]);
    assert!(running.status.success(), "{}", stderr(&running));
    let temporary = reuben(&b.0, &["describe", &source, "--temporary-server"]);
    assert!(temporary.status.success(), "{}", stderr(&temporary));
    assert_eq!(stdout(&running), stdout(&temporary));
    assert!(!stdout(&running).is_empty());
}

#[test]
fn a_running_server_is_named_on_stderr() {
    let a = TempDir::new("cli_named_a");
    let doc = a.seed("doc.json", "named");
    let root = a.0.display().to_string();
    let play = spawn_play(&a.0, &["--instrument-root", &root]);
    let source = doc.display().to_string();

    let running = reuben(&a.0, &["describe", &source, "--structure-addr", &play.addr]);
    assert!(
        stderr(&running).contains(&format!("answered by the reuben server at {}", play.addr)),
        "{}",
        stderr(&running)
    );
    let temporary = reuben(&a.0, &["describe", &source, "--temporary-server"]);
    assert!(
        !stderr(&temporary).contains("answered by"),
        "{}",
        stderr(&temporary)
    );
}

#[test]
fn a_relative_name_means_the_clis_directory() {
    let a = TempDir::new("cli_relative_a");
    let b = TempDir::new("cli_relative_b");
    a.seed("doc.json", "meant");
    b.seed("doc.json", "wrong");
    let root = a.0.display().to_string();
    let play = spawn_play(&b.0, &["--instrument-root", &root]);

    let out = reuben(
        &a.0,
        &[
            "describe",
            "doc.json",
            "--view",
            "boundary",
            "--structure-addr",
            &play.addr,
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("meant"), "{}", stdout(&out));
    assert!(!stdout(&out).contains("wrong"), "{}", stdout(&out));
}

#[test]
fn a_name_the_server_does_not_offer_is_refused_in_the_users_spelling() {
    let a = TempDir::new("cli_unoffered_a");
    let c = TempDir::new("cli_unoffered_c");
    c.seed("doc.json", "elsewhere");
    let root = a.0.display().to_string();
    let play = spawn_play(&a.0, &["--instrument-root", &root]);

    let out = reuben(
        &c.0,
        &["describe", "doc.json", "--structure-addr", &play.addr],
    );
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("`doc.json` is not offered by this server"),
        "{err}"
    );
    assert!(!err.contains(&c.0.display().to_string()), "{err}");
}

#[test]
fn describe_never_changes_what_plays() {
    let a = TempDir::new("cli_unchanged_a");
    let doc = a.seed("doc.json", "playing");
    let other = a.seed("other.json", "other");
    let source = doc.display().to_string();
    let play = spawn_play(&a.0, &[&source]);
    let before = channel(&play.addr).get_document().expect("a snapshot");

    for view in ["index", "boundary"] {
        let out = reuben(
            &a.0,
            &[
                "describe",
                &other.display().to_string(),
                "--view",
                view,
                "--structure-addr",
                &play.addr,
            ],
        );
        assert!(out.status.success(), "{view}: {}", stderr(&out));
    }

    let after = channel(&play.addr).get_document().expect("a snapshot");
    assert_eq!(after.source, before.source);
    assert_eq!(after.content_hash, before.content_hash);
}

#[test]
fn an_explicit_unreachable_address_does_not_fall_back() {
    let a = TempDir::new("cli_dead");
    let dead = format!("127.0.0.1:{}", common::free_tcp_port());
    let out = reuben(&a.0, &["describe", "--structure-addr", &dead]);
    assert!(!out.status.success());
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    assert!(stderr(&out).contains("reuben play"), "{}", stderr(&out));
}

#[test]
fn operator_listing_comes_from_the_server() {
    let a = TempDir::new("cli_operators");
    let play = spawn_play(&a.0, &[]);
    let out = reuben(
        &a.0,
        &["describe", "--json", "--structure-addr", &play.addr],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let operators: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("a JSON operator list");
    let names: Vec<&str> = operators
        .as_array()
        .expect("an array")
        .iter()
        .filter_map(|o| o["type_name"].as_str())
        .collect();
    assert!(names.contains(&"oscillator"), "{names:?}");
}

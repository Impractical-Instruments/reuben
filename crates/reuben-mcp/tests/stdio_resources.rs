//! Integration test for the MCP stdio resource surface (#319 verification, extended by R9 #466 to
//! the vocabulary + library-index resources): spawn the real shim binary, complete the
//! `initialize` handshake, and drive `resources/list` + `resources/read` over newline-delimited
//! JSON-RPC — the actual protocol boundary the client sees, not an in-process shortcut. Mirrors the
//! tool-surface harness in `stdio_tools_list.rs`, with the same watchdog so a regression fails
//! loudly instead of hanging CI.
//!
//! The tier-2 acceptance this file rides (ADR-0059 §8): resources served byte-equal to the
//! checkout, the retired instrument-JSON-Schema resource (ADR-0059 §4) absent, and every
//! `INSTRUCTIONS` pointer resolves.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

/// The committed checkout artifacts — compile-time bound so a deleted or stale file fails as
/// loudly as a byte mismatch. Same relative depth from `crates/reuben-mcp/tests/` as the
/// staleness tests that keep these files honest (`crates/reuben-core/tests/vocabulary.rs`,
/// `crates/reuben-native/tests/library_index.rs`).
const COMMITTED_VOCABULARY: &str = include_str!("../../../docs/agents/vocabulary.md");
const COMMITTED_LIBRARY_INDEX: &str = include_str!("../../../instruments/index.md");

/// The retired resource URI (ADR-0059 §4): the instrument JSON Schema resource, deleted outright.
/// Built by concatenation, like `no_dangling_references.rs`'s own retired tokens, so this literal
/// doesn't trip that test's live-text tripwire for the very machinery it proves absent here.
fn retired_schema_resource_uri() -> String {
    ["reuben://", "schema/instrument"].concat()
}

/// Drive the shim through initialize → initialized → the given requests over stdio and return the
/// raw stdout. Requests are buffered into the child's stdin, which is then closed; on EOF the shim
/// shuts down (ADR-0044 §1), flushing every response first, so reading stdout to EOF collects all
/// of them. A watchdog thread bounds the read so a protocol regression fails loudly.
fn drive(requests: &[&str]) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_reuben-mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the reuben-mcp binary");

    let mut stdin = child.stdin.take().expect("child stdin");
    // Minimal, spec-shaped JSON-RPC. The server negotiates the protocol version from ours.
    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"reuben-mcp-it","version":"0.0.0"}}}"#;
    let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    writeln!(stdin, "{initialize}").expect("write initialize");
    writeln!(stdin, "{initialized}").expect("write initialized");
    for request in requests {
        writeln!(stdin, "{request}").expect("write request");
    }
    stdin.flush().expect("flush stdin");
    drop(stdin); // EOF → the shim shuts down after draining the buffered requests.

    let mut stdout = child.stdout.take().expect("child stdout");
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        let _ = tx.send(out);
    });

    let out = match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(out) => out,
        Err(_) => {
            let _ = child.kill();
            panic!("reuben-mcp did not answer the resource requests within 30s");
        }
    };
    let _ = reader.join();
    let _ = child.wait();
    out
}

/// Pull the JSON-RPC response carrying the given `id` out of the shim's newline-delimited output.
fn response_with_id(out: &str, id: i64) -> Value {
    out.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|msg| msg.get("id") == Some(&serde_json::json!(id)))
        .unwrap_or_else(|| panic!("no JSON-RPC response with id {id} in shim output:\n{out}"))
}

#[test]
fn resources_list_advertises_guide_vocabulary_and_index_only() {
    let out = drive(&[r#"{"jsonrpc":"2.0","id":2,"method":"resources/list","params":{}}"#]);
    let response = response_with_id(&out, 2);

    let resources = response["result"]["resources"]
        .as_array()
        .unwrap_or_else(|| panic!("resources/list result missing a resources array:\n{response}"));

    // Index the advertised (uri -> mimeType) so the assertions read as a spec.
    let advertised: Vec<(String, String)> = resources
        .iter()
        .map(|r| {
            let uri = r["uri"].as_str().expect("each resource advertises a uri");
            let mime = r["mimeType"]
                .as_str()
                .unwrap_or_else(|| panic!("resource {uri} advertises no mimeType:\n{r}"));
            (uri.to_string(), mime.to_string())
        })
        .collect();

    // Exactly the guide + vocabulary + index — an exact-set assertion, so the retired
    // instrument-JSON-Schema resource (deleted outright, ADR-0059 §4) can never quietly
    // reappear on the wire, and no fourth resource sneaks in unnoticed either.
    assert_eq!(
        advertised,
        vec![
            (
                reuben_mcp::GUIDE_RESOURCE_URI.to_string(),
                reuben_mcp::GUIDE_RESOURCE_MIME.to_string(),
            ),
            (
                reuben_mcp::VOCABULARY_RESOURCE_URI.to_string(),
                reuben_mcp::VOCABULARY_RESOURCE_MIME.to_string(),
            ),
            (
                reuben_mcp::LIBRARY_INDEX_RESOURCE_URI.to_string(),
                reuben_mcp::LIBRARY_INDEX_RESOURCE_MIME.to_string(),
            ),
        ],
        "resources/list must advertise exactly guide + vocabulary + library index (ADR-0048 §7, \
         as amended by ADR-0059 §3/§6): {advertised:?}"
    );

    // Named explicitly too (ADR-0059 §8's tier-2 wording): the retired schema URI never appears.
    let schema_uri = retired_schema_resource_uri();
    assert!(
        !advertised.iter().any(|(uri, _)| *uri == schema_uri),
        "the retired {schema_uri} resource must never be advertised: {advertised:?}"
    );
}

/// Read `reuben://<uri>` over stdio and return the first contents block (uri, mimeType, text).
fn read_resource(uri: &str) -> (String, String, String) {
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"resources/read","params":{{"uri":"{uri}"}}}}"#
    );
    let out = drive(&[&request]);
    let response = response_with_id(&out, 3);
    let block = &response["result"]["contents"][0];
    let block_uri = block["uri"]
        .as_str()
        .unwrap_or_else(|| panic!("resources/read({uri}) contents[0] missing a uri:\n{response}"));
    let mime = block["mimeType"].as_str().unwrap_or_else(|| {
        panic!("resources/read({uri}) contents[0] missing a mimeType:\n{response}")
    });
    let text = block["text"]
        .as_str()
        .unwrap_or_else(|| panic!("resources/read({uri}) contents[0] is not text:\n{response}"));
    (block_uri.to_string(), mime.to_string(), text.to_string())
}

#[test]
fn read_authoring_guide_is_nonempty_markdown() {
    let (uri, mime, text) = read_resource(reuben_mcp::GUIDE_RESOURCE_URI);
    assert_eq!(uri, reuben_mcp::GUIDE_RESOURCE_URI);
    assert_eq!(mime, reuben_mcp::GUIDE_RESOURCE_MIME);
    assert!(
        !text.trim().is_empty(),
        "the authoring guide resource must be non-empty markdown"
    );
    assert!(
        text.contains('#'),
        "the authoring guide is markdown — it should carry at least one heading"
    );
}

#[test]
fn read_vocabulary_is_byte_equal_to_the_checkout() {
    // ADR-0059 §8 tier-2: the resource served over the wire is byte-equal with the checkout —
    // read from disk at request time (ADR-0051 §4), not baked into the binary, so this also
    // proves the crate isn't serving a stale embedded copy.
    let (uri, mime, text) = read_resource(reuben_mcp::VOCABULARY_RESOURCE_URI);
    assert_eq!(uri, reuben_mcp::VOCABULARY_RESOURCE_URI);
    assert_eq!(mime, reuben_mcp::VOCABULARY_RESOURCE_MIME);
    assert_eq!(
        text, COMMITTED_VOCABULARY,
        "the served vocabulary must be byte-equal to docs/agents/vocabulary.md"
    );
}

#[test]
fn read_library_index_is_byte_equal_to_the_checkout() {
    // ADR-0059 §8 tier-2: same byte-equal contract as the vocabulary, for the library index.
    let (uri, mime, text) = read_resource(reuben_mcp::LIBRARY_INDEX_RESOURCE_URI);
    assert_eq!(uri, reuben_mcp::LIBRARY_INDEX_RESOURCE_URI);
    assert_eq!(mime, reuben_mcp::LIBRARY_INDEX_RESOURCE_MIME);
    assert_eq!(
        text, COMMITTED_LIBRARY_INDEX,
        "the served library index must be byte-equal to instruments/index.md"
    );
}

#[test]
fn read_retired_schema_resource_is_a_resource_not_found_error() {
    // ADR-0059 §8 tier-2, named explicitly by the ticket: the retired schema resource URI is
    // absent — not just missing from `resources/list`, but genuinely unreadable.
    let schema_uri = retired_schema_resource_uri();
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":9,"method":"resources/read","params":{{"uri":"{schema_uri}"}}}}"#
    );
    let out = drive(&[&request]);
    let response = response_with_id(&out, 9);
    assert!(
        response.get("result").is_none(),
        "the retired schema resource must not return a result: {response}"
    );
    assert_eq!(
        response["error"]["code"],
        serde_json::json!(-32002),
        "reading the retired schema resource must be resource_not_found: {response}"
    );
}

#[test]
fn every_instructions_pointer_resolves() {
    // ADR-0059 §8 tier-2, named explicitly by the ticket: every `reuben://` URI the wire
    // `instructions` text names must actually resolve over `resources/read`. Reads the live
    // `instructions` off the real `initialize` response — not the private INSTRUCTIONS constant —
    // so this proves the wire contract, not just the source string.
    let out = drive(&[]);
    let response = response_with_id(&out, 1);
    let instructions = response["result"]["instructions"]
        .as_str()
        .unwrap_or_else(|| panic!("initialize result missing `instructions`: {response}"));

    let pointers: Vec<&str> = instructions
        .split("reuben://")
        .skip(1) // the text before the first `reuben://` is not a pointer
        .map(|tail| {
            let end = tail
                .find(|c: char| c.is_whitespace() || c == '`' || c == '.' || c == ',')
                .unwrap_or(tail.len());
            &tail[..end]
        })
        .collect();
    assert!(
        !pointers.is_empty(),
        "the instructions text should name at least one reuben:// pointer: {instructions:?}"
    );

    for pointer in pointers {
        let uri = format!("reuben://{pointer}");
        let (resolved_uri, _, text) = read_resource(&uri);
        assert_eq!(
            resolved_uri, uri,
            "the instructions pointer {uri} must resolve to itself"
        );
        assert!(
            !text.trim().is_empty(),
            "the instructions pointer {uri} must resolve to non-empty content"
        );
    }
}

#[test]
fn read_unknown_resource_is_a_resource_not_found_error() {
    // ADR-0048 §7: an unknown resource URI is `resource_not_found` — not a silent empty read, not a
    // panic. Drives the `other =>` arm of `read_resource` (otherwise untested) over the real stdio
    // JSON-RPC boundary and asserts a protocol error, naming the URIs the server does serve.
    let request = r#"{"jsonrpc":"2.0","id":7,"method":"resources/read","params":{"uri":"reuben://does/not/exist"}}"#;
    let out = drive(&[request]);
    let response = response_with_id(&out, 7);
    assert!(
        response.get("result").is_none(),
        "an unknown URI must not return a result: {response}"
    );
    let error = response.get("error").unwrap_or_else(|| {
        panic!("an unknown resource URI must surface a JSON-RPC error: {response}")
    });
    // MCP's resource-not-found code (JSON-RPC application range), and a message that both names the
    // offending URI and points at what IS served — so the agent can correct the request.
    assert_eq!(
        error["code"],
        serde_json::json!(-32002),
        "resource-not-found uses the MCP resource_not_found code: {response}"
    );
    let message = error["message"].as_str().unwrap_or("");
    assert!(
        message.contains("reuben://does/not/exist"),
        "the error should name the unknown URI: {response}"
    );
    assert!(
        message.contains(reuben_mcp::GUIDE_RESOURCE_URI),
        "the error should point at the URI the server does serve: {response}"
    );
}

//! Integration test for the MCP stdio resource surface (#319 verification): spawn the real shim
//! binary, complete the `initialize` handshake, and drive `resources/list` + `resources/read` over
//! newline-delimited JSON-RPC — the actual protocol boundary the client sees, not an in-process
//! shortcut. Mirrors the tool-surface harness in `stdio_tools_list.rs`, with the same watchdog so a
//! regression fails loudly instead of hanging CI.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

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
fn resources_list_advertises_two_uris() {
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

    assert_eq!(
        advertised.len(),
        2,
        "resources/list must advertise exactly the two static resources (ADR-0048 §7): {advertised:?}"
    );
    assert!(
        advertised.contains(&(
            reuben_mcp::SCHEMA_RESOURCE_URI.to_string(),
            reuben_mcp::SCHEMA_RESOURCE_MIME.to_string(),
        )),
        "resources/list must advertise {} as {}: {advertised:?}",
        reuben_mcp::SCHEMA_RESOURCE_URI,
        reuben_mcp::SCHEMA_RESOURCE_MIME,
    );
    assert!(
        advertised.contains(&(
            reuben_mcp::GUIDE_RESOURCE_URI.to_string(),
            reuben_mcp::GUIDE_RESOURCE_MIME.to_string(),
        )),
        "resources/list must advertise {} as {}: {advertised:?}",
        reuben_mcp::GUIDE_RESOURCE_URI,
        reuben_mcp::GUIDE_RESOURCE_MIME,
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
fn read_schema_resource_parses_as_json_schema() {
    let (uri, mime, text) = read_resource(reuben_mcp::SCHEMA_RESOURCE_URI);
    assert_eq!(uri, reuben_mcp::SCHEMA_RESOURCE_URI);
    assert_eq!(mime, reuben_mcp::SCHEMA_RESOURCE_MIME);

    // The body is the instrument JSON Schema: it deserializes, and carries the two markers of a
    // JSON Schema document — a `$schema` dialect and a top-level `type`.
    let schema: Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("schema body is not JSON: {e}\n{text}"));
    assert!(
        schema["$schema"].is_string(),
        "served schema must declare a `$schema` dialect: {schema}"
    );
    assert_eq!(
        schema["type"], "object",
        "served schema must have a top-level object `type`: {schema}"
    );
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
        message.contains(reuben_mcp::GUIDE_RESOURCE_URI)
            && message.contains(reuben_mcp::SCHEMA_RESOURCE_URI),
        "the error should point at the URIs the server does serve: {response}"
    );
}

/// Drift guard (#319): the schema SERVED over `reuben://schema/instrument` must equal the committed
/// `crates/reuben-core/schema/instrument.schema.json` byte-for-byte. The served copy is generated
/// live from the registry, so if an operator change makes the two diverge this fails — forcing
/// `cargo run -p reuben-core --example gen_schema` — and the served schema can never be stale.
#[test]
fn read_schema_resource_matches_committed() {
    let (_, _, served) = read_resource(reuben_mcp::SCHEMA_RESOURCE_URI);
    let committed = include_str!("../../reuben-core/schema/instrument.schema.json");
    assert_eq!(
        served, committed,
        "the served instrument schema has drifted from the committed \
         crates/reuben-core/schema/instrument.schema.json — regenerate it with \
         `cargo run -p reuben-core --example gen_schema`"
    );
}

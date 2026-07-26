//! Integration test for the MCP stdio wire surface: spawn the real shim binary, complete the
//! `initialize` handshake, and read `tools/list` over newline-delimited JSON-RPC to assert what only
//! the wire can answer — every roster verb carries an `outputSchema` and the window's own sentence —
//! then scan every advertised description for markup that only a Rust reader can resolve.
//!
//! What is *not* here is a roster parity test. It used to be, and it is the door's construction that
//! replaced it: `stamp_window_prose` walks the built router against
//! `reuben_api::tools::CONTRACTS` in both directions and refuses to start on a mismatch, so a
//! surface that is not the roster cannot reach this wire to be observed.
//!
//! see rules: agent-mcp, code-as-grounding

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Drive the shim through initialize → initialized → `extra` requests over stdio and return the
/// raw stdout. Requests are buffered into the child's stdin, which is then closed; on EOF the shim
/// shuts down, flushing every response first, so reading stdout to EOF collects all
/// of them. A watchdog thread bounds the read so a protocol regression fails loudly instead of
/// hanging CI.
fn drive(extra: &[&str]) -> String {
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
    for request in extra {
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
            panic!("reuben-mcp did not answer initialize + {extra:?} within 30s");
        }
    };
    let _ = reader.join();
    let _ = child.wait();
    out
}

const TOOLS_LIST: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
const RESOURCES_LIST: &str = r#"{"jsonrpc":"2.0","id":3,"method":"resources/list","params":{}}"#;

/// The JSON-RPC response carrying `id`, or a panic naming the whole stdout.
fn response_with_id(out: &str, id: u64) -> serde_json::Value {
    out.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|msg| msg.get("id") == Some(&serde_json::json!(id)))
        .unwrap_or_else(|| panic!("no response with id {id} in shim output:\n{out}"))
}

#[test]
fn every_tool_advertises_an_output_schema() {
    // Every tool declares an `outputSchema` (rmcp derives it from the window's own result types
    // via schemars). Asserting the whole roster over the wire also proves the shim STARTS — the
    // engine tools' `schema_for_output` calls run at router construction, so a schema that failed
    // to derive would panic the binary before it could answer this request.
    let out = drive(&[TOOLS_LIST]);
    let response = response_with_id(&out, 2);
    let tools = response["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list result missing a tools array:\n{response}"));

    for name in reuben_mcp::tool_names() {
        let tool = tools
            .iter()
            .find(|t| t["name"] == serde_json::json!(name))
            .unwrap_or_else(|| panic!("tools/list missing `{name}`"));
        assert!(
            tool["outputSchema"].is_object(),
            "`{name}` must advertise an outputSchema: {tool}"
        );
    }
}

#[test]
fn advertises_the_window_prose() {
    // The window owns every verb's sentence, and the door stamps it onto the built router
    // because rmcp's `#[tool]` takes only a literal. Left unstamped, the macro falls back to the
    // method's rustdoc — prose written for a Rust reader, and a silent regression the roster and
    // schema tests would both pass through. So this reads the real wire and demands the window's
    // string exactly. see rules: agent-mcp
    let out = drive(&[TOOLS_LIST]);
    let response = response_with_id(&out, 2);
    let tools = response["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list result missing a tools array:\n{response}"));

    for contract in reuben_api::tools::CONTRACTS {
        let name = contract.name;
        let tool = tools
            .iter()
            .find(|t| t["name"] == serde_json::json!(name))
            .unwrap_or_else(|| panic!("tools/list missing `{name}`"));
        assert_eq!(
            tool["description"].as_str(),
            Some(contract.description),
            "`{name}` must advertise the window's sentence, not a door-local copy"
        );
    }
}

/// The banned markup, as (label, detector). Hand-rolled rather than a regex dependency: the three
/// shapes are each a single scan.
type Detector = (&'static str, fn(&str) -> bool);

const BANNED: [Detector; 3] = [
    ("rustdoc link syntax, e.g. [`Foo`] or [`x`](path)", |s| {
        s.match_indices("[`")
            .any(|(i, _)| s[i + 2..].contains("`]"))
    }),
    ("an issue number, e.g. #608", |s| {
        s.match_indices('#')
            .any(|(i, _)| s[i + 1..].chars().take_while(char::is_ascii_digit).count() >= 2)
    }),
    ("an internal crate path, e.g. crate::x", |s| {
        s.match_indices("::").any(|(i, _)| {
            let before = s[..i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
            let after = s[i + 2..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
            before && after
        })
    }),
];

/// Collect every `description` reachable in `node`, tagged with its JSON path.
fn descriptions(node: &serde_json::Value, path: &str, out: &mut Vec<(String, String)>) {
    match node {
        serde_json::Value::Object(map) => {
            if let Some(d) = map.get("description").and_then(|d| d.as_str()) {
                out.push((path.to_string(), d.to_string()));
            }
            for (k, v) in map {
                descriptions(v, &format!("{path}/{k}"), out);
            }
        }
        serde_json::Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                descriptions(v, &format!("{path}/{i}"), out);
            }
        }
        _ => {}
    }
}

#[test]
fn advertised_prose_is_model_facing() {
    // Reads what the door advertises rather than what a type declares, so it cannot drift from the
    // wire: schema `description`s are generated from doc comments, and the model on the other end
    // can resolve none of the rustdoc markup a Rust reader is served by.
    // see rules: code-as-grounding
    let out = drive(&[RESOURCES_LIST, TOOLS_LIST]);

    let mut advertised = Vec::new();
    descriptions(
        &response_with_id(&out, 2)["result"],
        "tools/list",
        &mut advertised,
    );
    descriptions(
        &response_with_id(&out, 3)["result"],
        "resources/list",
        &mut advertised,
    );
    let init = response_with_id(&out, 1);
    let instructions = init["result"]["instructions"]
        .as_str()
        .expect("initialize advertises server instructions");
    advertised.push((
        "initialize/instructions".to_string(),
        instructions.to_string(),
    ));

    // A shim that advertised nothing would pass every assertion below vacuously.
    assert!(
        advertised.len() > 100,
        "expected the whole advertised prose surface, got {} entries",
        advertised.len()
    );

    let offenders: Vec<String> = advertised
        .iter()
        .flat_map(|(path, text)| {
            BANNED
                .iter()
                .filter(|(_, hits)| hits(text))
                .map(move |(label, _)| format!("  {path}\n    carries {label}\n    in: {text}"))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "an advertised description is model-facing prose, not a comment — no rustdoc link syntax, \
         no issue numbers, no internal crate paths:\n{}",
        offenders.join("\n")
    );
}

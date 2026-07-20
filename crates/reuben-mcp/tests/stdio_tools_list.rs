//! Integration test for the MCP stdio wire surface (#313 verification): spawn the real shim
//! binary, complete the `initialize` handshake, and assert `tools/list` advertises exactly the
//! declared contract roster over newline-delimited JSON-RPC — the actual protocol
//! boundary the client sees, not an in-process shortcut. The expected set is derived from the
//! single-source `reuben_core::tools::CONTRACTS` roster (#157), not hand-typed here.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Drive the shim through initialize → initialized → tools/list over stdio and return the raw
/// stdout. Requests are buffered into the child's stdin, which is then closed; on EOF the shim
/// shuts down, flushing every response first, so reading stdout to EOF collects all
/// of them. A watchdog thread bounds the read so a protocol regression fails loudly instead of
/// hanging CI.
fn drive_tools_list() -> String {
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
    let tools_list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
    writeln!(stdin, "{initialize}").expect("write initialize");
    writeln!(stdin, "{initialized}").expect("write initialized");
    writeln!(stdin, "{tools_list}").expect("write tools/list");
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
            panic!("reuben-mcp did not answer initialize + tools/list within 30s");
        }
    };
    let _ = reader.join();
    let _ = child.wait();
    out
}

#[test]
fn advertises_the_declared_roster_over_stdio() {
    let out = drive_tools_list();

    // The tools/list response is the JSON-RPC message carrying id == 2.
    let response = out
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|msg| msg.get("id") == Some(&serde_json::json!(2)))
        .unwrap_or_else(|| panic!("no tools/list response (id 2) in shim output:\n{out}"));

    let advertised: Vec<String> = response["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list result missing a tools array:\n{response}"))
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .expect("each tool advertises a string name")
                .to_string()
        })
        .collect();

    for expected in reuben_mcp::tool_names() {
        assert!(
            advertised.iter().any(|name| name == expected),
            "tools/list is missing `{expected}`; advertised: {advertised:?}"
        );
    }
    assert_eq!(
        advertised.len(),
        reuben_mcp::tool_names().len(),
        "tools/list must advertise exactly the declared contract roster, got: {advertised:?}"
    );
}

#[test]
fn every_tool_advertises_an_output_schema() {
    // Every tool declares an `outputSchema` (rmcp derives it from the contract types
    // via schemars). Asserting the whole roster over the wire also proves the shim STARTS — the
    // engine tools' `schema_for_output` calls run at router construction, so a schema that failed
    // to derive would panic the binary before it could answer this request.
    let out = drive_tools_list();
    let response = out
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|msg| msg.get("id") == Some(&serde_json::json!(2)))
        .unwrap_or_else(|| panic!("no tools/list response (id 2) in shim output:\n{out}"));
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

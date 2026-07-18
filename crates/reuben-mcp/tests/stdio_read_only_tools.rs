//! Integration tests for the read-only tools over stdio (#316 verification): spawn the real
//! shim binary, complete the `initialize` handshake, and drive a `tools/call` for
//! `describe_operators` / `describe_instrument` / `validate` over newline-delimited JSON-RPC —
//! the actual protocol boundary the client sees, not an in-process shortcut.
//!
//! These assert the error-layer discipline (ADR-0048 §3): `isError` is reserved for
//! can't-do-the-job cases (unreadable path, ambiguous one-of, unknown operator, a document with
//! no boundary to describe), while a *failing validation* is an ordinary result carrying an
//! `ok:false` report. Every call is bounded by a watchdog so a protocol regression fails loudly
//! instead of hanging CI.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Drive the shim through initialize → initialized → a single `tools/call` and return the raw
/// stdout. Requests are buffered into the child's stdin, which is then closed; on EOF the shim
/// shuts down (ADR-0044 §1), flushing every response first. A watchdog thread bounds the read so
/// a regression fails loudly instead of hanging CI.
fn drive_tool_call(name: &str, arguments: serde_json::Value) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_reuben-mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the reuben-mcp binary");

    let mut stdin = child.stdin.take().expect("child stdin");
    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"reuben-mcp-it","version":"0.0.0"}}}"#;
    let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    });
    writeln!(stdin, "{initialize}").expect("write initialize");
    writeln!(stdin, "{initialized}").expect("write initialized");
    writeln!(stdin, "{call}").expect("write tools/call");
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
            panic!("reuben-mcp did not answer initialize + tools/call within 30s");
        }
    };
    let _ = reader.join();
    let _ = child.wait();
    out
}

/// Drive one `tools/call` and return the JSON-RPC `result` object (the [`CallToolResult`]).
fn call_tool(name: &str, arguments: serde_json::Value) -> serde_json::Value {
    let out = drive_tool_call(name, arguments);
    let response = out
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|msg| msg.get("id") == Some(&serde_json::json!(2)))
        .unwrap_or_else(|| panic!("no tools/call response (id 2) in shim output:\n{out}"));
    response
        .get("result")
        .cloned()
        .unwrap_or_else(|| panic!("tools/call response carried no result:\n{response}"))
}

/// `isError == true` on the result.
fn is_error(result: &serde_json::Value) -> bool {
    result["isError"] == serde_json::json!(true)
}

#[test]
fn describe_operators_unknown_name_is_iserror() {
    // ADR-0048 §5: an unknown operator name is a can't-do-the-job error — the tool cannot
    // describe an operator that does not exist.
    let result = call_tool(
        "describe_operators",
        serde_json::json!({ "name": "definitely_not_an_operator" }),
    );
    assert!(
        is_error(&result),
        "unknown operator name must be isError: {result}"
    );
}

#[test]
fn describe_operators_no_filter_lists_all() {
    // ADR-0048 §5: no filter mirrors `introspect::describe(None)` — every registered operator,
    // structured under `{ operators: [...] }`. The count must match the live registry.
    let result = call_tool("describe_operators", serde_json::json!({}));
    assert!(
        !is_error(&result),
        "listing all operators is not an error: {result}"
    );
    let operators = result["structuredContent"]["operators"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("describe_operators must return {{ operators: [...] }}: {result}")
        });
    let expected = reuben_core::Registry::builtin().entries().count();
    assert_eq!(
        operators.len(),
        expected,
        "describe_operators must list exactly the builtin registry ({expected}): {result}"
    );
    // A human-readable text block accompanies the structured content (ADR-0048 §3).
    assert!(
        result["content"][0]["text"].is_string(),
        "the result must carry a human-readable text block: {result}"
    );
}

#[test]
fn describe_operators_compact_returns_signatures() {
    // ADR-0059 §3 (reuben#459): `compact:true` switches the verb to its generated signature-line
    // projection — `{ signatures: [...] }`, one line per registered operator, with the full port
    // objects absent (their token weight is the point). Same registry truth, same count.
    let result = call_tool("describe_operators", serde_json::json!({ "compact": true }));
    assert!(
        !is_error(&result),
        "compact listing is not an error: {result}"
    );

    let signatures = result["structuredContent"]["signatures"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("compact describe_operators must return {{ signatures: [...] }}: {result}")
        });
    let expected = reuben_core::Registry::builtin().entries().count();
    assert_eq!(
        signatures.len(),
        expected,
        "compact mode must list exactly the builtin registry ({expected}): {result}"
    );
    assert!(
        result["structuredContent"].get("operators").is_none(),
        "compact mode must not also ship the full port objects: {result}"
    );
    // Spot-check the notation on a known line: name(inputs…) -> outputs.
    assert!(
        signatures
            .iter()
            .any(|s| s.as_str().is_some_and(|s| s.starts_with("filter(")
                && s.contains("cutoff:signal")
                && s.contains("-> audio:signal"))),
        "the filter signature must carry the wiring essentials: {result}"
    );
}

#[test]
fn validate_broken_doc_is_ok_false_not_iserror() {
    // ADR-0048 §3, the crux: a failing validation is the tool *working*. An inline document with a
    // typo'd operator type validates to `ok:false` with a node-named Diag — an ordinary result,
    // NOT isError.
    let doc = serde_json::json!({
        "instrument": "typo",
        "nodes": [ { "type": "oscilllator", "address": "/osc" } ],
        "outputs": []
    });
    let result = call_tool("validate", serde_json::json!({ "document": doc }));
    assert!(
        !is_error(&result),
        "a failed validation is an ordinary result, never isError: {result}"
    );
    let report = &result["structuredContent"];
    assert_eq!(
        report["ok"],
        serde_json::json!(false),
        "the broken document must validate to ok:false: {result}"
    );
    assert_eq!(
        report["errors"][0]["node"],
        serde_json::json!("/osc"),
        "the error Diag must localize the offending node: {result}"
    );
}

#[test]
fn describe_instrument_unloadable_is_iserror() {
    // ADR-0048 §3 corollary: a document that fails to load has no boundary to describe, so
    // describe_instrument is isError (the message points the user at `validate`).
    let doc = serde_json::json!({
        "instrument": "typo",
        "nodes": [ { "type": "oscilllator", "address": "/osc" } ],
        "outputs": []
    });
    let result = call_tool(
        "describe_instrument",
        serde_json::json!({ "document": doc }),
    );
    assert!(
        is_error(&result),
        "an unloadable document must be isError for describe_instrument: {result}"
    );
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("isError result must carry guidance text: {result}"));
    assert!(
        text.contains("validate"),
        "the guidance must point the user at `validate`: {text}"
    );
}

#[test]
fn one_of_path_and_document_both_present_is_iserror() {
    // ADR-0048 §2: exactly one of `path` or `document`. Both present is an ambiguous one-of —
    // a can't-do-the-job error, not a deliverable.
    let doc = serde_json::json!({ "instrument": "x", "nodes": [], "outputs": [] });
    let result = call_tool(
        "validate",
        serde_json::json!({ "path": "some/instrument.json", "document": doc }),
    );
    assert!(
        is_error(&result),
        "both path and document present must be isError: {result}"
    );
}

#[test]
fn one_of_neither_path_nor_document_is_iserror() {
    // The other half of the one-of (ADR-0048 §2): neither given is equally unworkable.
    let result = call_tool("validate", serde_json::json!({}));
    assert!(
        is_error(&result),
        "neither path nor document present must be isError: {result}"
    );
}

/// Drive initialize → initialized → tools/list and return the raw stdout, watchdog-bounded.
fn drive_tools_list() -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_reuben-mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the reuben-mcp binary");

    let mut stdin = child.stdin.take().expect("child stdin");
    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"reuben-mcp-it","version":"0.0.0"}}}"#;
    let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    let tools_list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
    writeln!(stdin, "{initialize}").expect("write initialize");
    writeln!(stdin, "{initialized}").expect("write initialized");
    writeln!(stdin, "{tools_list}").expect("write tools/list");
    stdin.flush().expect("flush stdin");
    drop(stdin);

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
fn read_only_tools_advertise_output_schemas() {
    // ADR-0048 §3: every tool declares an `outputSchema`. The three read-only tools derive theirs
    // from the introspect/contract types via schemars (the `schemars` fence), so a client can
    // validate the structured content it gets back.
    let out = drive_tools_list();
    let response = out
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|msg| msg.get("id") == Some(&serde_json::json!(2)))
        .unwrap_or_else(|| panic!("no tools/list response (id 2):\n{out}"));
    let tools = response["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list missing a tools array:\n{response}"));

    // The read-only tools are exactly the Pure contracts (ADR-0048 §1) — derived from the
    // single-source roster (#157), not a hand-typed list, so Wave 2 adds a CONTRACTS entry rather
    // than editing a parallel literal here.
    let read_only = reuben_core::tools::CONTRACTS
        .iter()
        .filter(|c| c.kind == reuben_core::tools::ContractKind::Pure)
        .map(|c| c.name);
    for name in read_only {
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

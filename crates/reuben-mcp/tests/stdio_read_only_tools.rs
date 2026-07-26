//! Integration tests for the read-only tools over stdio: spawn the real shim binary, complete the
//! `initialize` handshake, and drive a `tools/call` over newline-delimited JSON-RPC — the actual
//! protocol boundary a client sees, not an in-process shortcut.
//!
//! Every case seeds a real file, since a document is always named by `source`, and every call is
//! bounded by a watchdog so a protocol regression fails loudly instead of hanging CI.
//!
//! see rules: agent-mcp

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Drive the shim through initialize → initialized → a single `tools/call` and return the raw
/// stdout. Requests are buffered into the child's stdin, which is then closed; on EOF the shim
/// shuts down, flushing every response first. A watchdog thread bounds the read so
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

/// How many operators the registry holds, asked through the window rather than the engine — the
/// same question `describe_operators` answers, so the expectation cannot drift from the verb by
/// reaching around it.
fn registered_operator_count() -> usize {
    reuben_api::authoring::describe_operators(&reuben_api::authoring::DescribeOperators {
        name: None,
        compact: true,
    })
    .expect("listing every operator is not a refusal")
    .output
    .signatures
    .expect("compact mode answers with signatures")
    .len()
}

#[test]
fn describe_operators_unknown_name_is_iserror() {
    // An unknown operator name is a can't-do-the-job error — the tool cannot
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
    // No filter mirrors `introspect::describe(None)` — every registered operator,
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
    let expected = registered_operator_count();
    assert_eq!(
        operators.len(),
        expected,
        "describe_operators must list exactly the builtin registry ({expected}): {result}"
    );
    // A human-readable text block accompanies the structured content.
    assert!(
        result["content"][0]["text"].is_string(),
        "the result must carry a human-readable text block: {result}"
    );
}

#[test]
fn describe_operators_compact_returns_signatures() {
    // `compact:true` switches the verb to its signature-line projection: one line per registered
    // operator, with the full port objects absent (their token weight is the point).
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
    let expected = registered_operator_count();
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

/// Write `document` into a fresh temp directory and return its path — the only way to hand these
/// tools a document, since there is no inline arm.
fn seeded(case: &str, document: serde_json::Value) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("reuben_mcp_read_only_{case}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the case directory");
    let path = dir.join("instrument.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&document).expect("serialize"),
    )
    .expect("seed");
    path
}

/// A document that mints but does not load: the operator type is typo'd.
fn typo_document() -> serde_json::Value {
    serde_json::json!({
        "instrument": "typo",
        "nodes": [ { "type": "oscilllator", "address": "/osc" } ],
        "outputs": []
    })
}

#[test]
fn validate_broken_doc_is_ok_false_not_iserror() {
    // The crux: a failing validation is the tool *working*. A document with a typo'd operator type
    // validates to `ok:false` with a node-named Diag — an ordinary result, NOT isError.
    let path = seeded("validate_broken", typo_document());
    let result = call_tool(
        "validate_instrument",
        serde_json::json!({ "source": path.to_string_lossy() }),
    );
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
fn describe_instrument_projects_an_unloadable_document_but_has_no_boundary_for_it() {
    // Two different answers about the same broken document: the structural views still project
    // it, but the boundary view cannot be cut from a document that will not load.
    let path = seeded("describe_unloadable", typo_document());
    let source = serde_json::json!({ "source": path.to_string_lossy() });
    let result = call_tool("describe_instrument", source.clone());
    assert!(
        !is_error(&result),
        "an unloadable document still has structure to read: {result}"
    );
    let text = result["structuredContent"]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("the index view must carry rendered text: {result}"));
    assert!(
        text.contains("/osc"),
        "the index projects the nodes it has: {text}"
    );

    // ...but the *boundary* is the resolved face a host sees, and a document that will not load has
    // none. That case stays isError, pointing at the authority and at the view that does answer.
    let mut boundary = source;
    boundary["view"] = serde_json::json!("boundary");
    let result = call_tool("describe_instrument", boundary);
    assert!(
        is_error(&result),
        "an unloadable document has no boundary to describe: {result}"
    );
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("isError result must carry guidance text: {result}"));
    assert!(
        text.contains("validate_instrument"),
        "the guidance must point at the validation authority: {text}"
    );
}

#[test]
fn an_incoherent_selection_is_refused_rather_than_silently_narrowed() {
    // The selection grammar is core's (`Selection::from_terms`) precisely so both doors answer
    // this the same way. Silently honouring one field and dropping the other is the trap: the
    // caller that meant the dropped one gets a plausible answer to a question it did not ask.
    let path = seeded("incoherent_selection", typo_document());
    let source = path.to_string_lossy().to_string();

    let both = call_tool(
        "describe_instrument",
        serde_json::json!({
            "source": source, "view": "nodes", "select": ["/osc"], "type": "oscillator"
        }),
    );
    assert!(
        is_error(&both),
        "select and type together must be refused, not resolved by precedence: {both}"
    );

    // Same rule one branch further in: `boundary` builds no selection at all, so terms it cannot
    // honour must be refused there too rather than quietly ignored.
    let boundary = call_tool(
        "describe_instrument",
        serde_json::json!({ "source": source, "view": "boundary", "select": ["/osc"] }),
    );
    assert!(
        is_error(&boundary),
        "the boundary view takes no selection; terms it ignores must be an error: {boundary}"
    );
}

#[test]
fn a_missing_source_is_iserror_and_there_is_no_inline_document_arm() {
    // An unreadable `source` is the only can't-do-the-job shape left, and `document` is not a
    // field — passing one is a schema violation, not a second way in. Neither may quietly succeed.
    let missing = call_tool(
        "validate_instrument",
        serde_json::json!({ "source": "definitely/not/here.json" }),
    );
    assert!(
        is_error(&missing),
        "an unreadable source must be isError: {missing}"
    );

    let inline = call_tool(
        "validate_instrument",
        serde_json::json!({ "document": typo_document() }),
    );
    assert!(
        is_error(&inline),
        "there is no inline `document` arm to validate through: {inline}"
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
    // Every tool declares an `outputSchema`. The three read-only tools derive theirs
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

    // The read-only tools are exactly the Pure contracts, derived from the single-source roster
    // rather than a hand-typed list — so a new tool is a CONTRACTS entry rather
    // than editing a parallel literal here.
    let read_only = reuben_api::tools::CONTRACTS
        .iter()
        .filter(|c| c.kind == reuben_api::tools::ContractKind::Pure)
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

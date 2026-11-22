//! Integration tests for the read-only tools over stdio: spawn the real shim binary, complete the
//! `initialize` handshake, and drive a `tools/call` over newline-delimited JSON-RPC — the actual
//! protocol boundary a client sees, not an in-process shortcut.
//!
//! No engine runs here: the sidecar is pointed at a loopback port nothing listens on, so a
//! `reuben play` on the same machine can never answer. What a document read returns from a real
//! engine is `native_engine`'s subject; here it is that, with none, the sidecar refuses rather
//! than read the file itself. Every call is bounded by a watchdog so a protocol regression fails
//! loudly instead of hanging CI.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use reuben_api::engine::ENGINE_UNREACHABLE_GUIDANCE;
use reuben_api::tools::names;
use reuben_mcp::ENGINE_ADDR_ENV;

/// A loopback address nothing listens on: bound for a free port, then released.
fn dead_addr() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind to reserve a port");
    let addr = listener.local_addr().expect("reserved addr");
    drop(listener);
    addr.to_string()
}

/// Drive the shim through initialize → initialized → a single `tools/call` and return the raw
/// stdout. Requests are buffered into the child's stdin, which is then closed; on EOF the shim
/// shuts down, flushing every response first. A watchdog thread bounds the read so
/// a regression fails loudly instead of hanging CI.
fn drive_tool_call(name: &str, arguments: serde_json::Value) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_reuben-mcp"))
        .env(ENGINE_ADDR_ENV, dead_addr())
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
        names::DESCRIBE_OPERATORS,
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
    let result = call_tool(names::DESCRIBE_OPERATORS, serde_json::json!({}));
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
    let result = call_tool(
        names::DESCRIBE_OPERATORS,
        serde_json::json!({ "compact": true }),
    );
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

#[test]
fn document_reads_with_no_engine_are_iserror_with_guidance() {
    // The engine owns the files. With none running, a document read is a can't-do-the-job
    // isError naming the fix — never an answer the sidecar made by reading the file itself.
    let dir = std::env::temp_dir().join(format!("reuben_mcp_read_only_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the case directory");
    let path = dir.join("instrument.json");
    std::fs::write(
        &path,
        r#"{ "format_version": 3, "instrument": "t", "nodes": [] }"#,
    )
    .expect("seed");
    let source = serde_json::json!({ "source": path.to_string_lossy() });
    for tool in [names::DESCRIBE_INSTRUMENT, names::VALIDATE_INSTRUMENT] {
        let result = call_tool(tool, source.clone());
        assert!(is_error(&result), "{tool}: {result}");
        assert_eq!(
            result["content"][0]["text"],
            serde_json::json!(ENGINE_UNREACHABLE_GUIDANCE),
            "{tool}: {result}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn there_is_no_inline_document_arm() {
    // `document` is not a field: passing one in place of `source` is refused at the argument
    // boundary, before anything is asked of an engine.
    let inline = call_tool(
        names::VALIDATE_INSTRUMENT,
        serde_json::json!({ "document": { "instrument": "t", "nodes": [] } }),
    );
    assert!(
        is_error(&inline),
        "there is no inline `document` arm to validate through: {inline}"
    );
    assert_ne!(
        inline["content"][0]["text"],
        serde_json::json!(ENGINE_UNREACHABLE_GUIDANCE),
        "the arguments are refused before an engine is dialed: {inline}"
    );
}

/// Drive initialize → initialized → tools/list and return the raw stdout, watchdog-bounded.
fn drive_tools_list() -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_reuben-mcp"))
        .env_remove(ENGINE_ADDR_ENV)
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

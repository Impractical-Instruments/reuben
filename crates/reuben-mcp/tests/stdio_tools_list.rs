//! Integration test for the MCP stdio wire surface: spawn the real shim binary, complete the
//! `initialize` handshake, and read `tools/list` over newline-delimited JSON-RPC to assert what only
//! the wire can answer — every roster verb carries an `outputSchema` and the window's own sentence,
//! and every argument slot it advertises constrains its value — then scan every advertised
//! description for markup that only a Rust reader can resolve.
//!
//! There is no roster check here: `stamp_window_prose` refuses to construct the server unless the
//! router and the roster are the same name-set, so a surface that is not the roster never reaches
//! this wire to be observed.
//!
//! see rules: agent-mcp, code-as-grounding

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use reuben_api::tools::names;

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
    // method's rustdoc — prose written for a Rust reader, and a silent regression the schema test
    // above passes straight through, since it iterates names and never reads a sentence. So this
    // reads the real wire and demands the window's string exactly. see rules: agent-mcp
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

/// A node input and an interface pipe's seed are one concept, so exactly one verb writes it. The
/// meta verb is the pipe's *quantity* contract and nothing else; the value verb owns the seed and
/// says how a pipe is addressed. Asserted on the wire because that is where a model reads it, and
/// because no completeness guard can catch a concept expressed twice.
#[test]
fn the_value_verb_owns_the_seed_and_the_meta_verb_is_the_quantity_contract() {
    let out = drive(&[TOOLS_LIST]);
    let response = response_with_id(&out, 2);
    let tools = response["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list result missing a tools array:\n{response}"));
    let tool = |name: &str| {
        tools
            .iter()
            .find(|t| t["name"] == serde_json::json!(name))
            .unwrap_or_else(|| panic!("tools/list missing `{name}`"))
    };

    let meta = tool(names::SET_INSTRUMENT_INTERFACE_INPUT_META);
    let properties = meta["inputSchema"]["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("no input properties on the meta verb: {meta}"));
    assert!(
        !properties.contains_key("default"),
        "the meta verb is the quantity contract (channel/min/max/curve/unit) — the seed is \
         set_instrument_input's: {meta}"
    );
    for field in ["channel", "min", "max", "curve", "unit"] {
        assert!(
            properties.contains_key(field),
            "the meta verb still carries `{field}`: {meta}"
        );
    }
    assert!(
        !meta["description"]
            .as_str()
            .expect("a described verb")
            .contains("default"),
        "the meta verb's sentence no longer advertises a seed it cannot write: {meta}"
    );

    let value = tool(names::SET_INSTRUMENT_INPUT)["description"]
        .as_str()
        .expect("a described verb");
    assert!(
        value.contains("pipe"),
        "the value verb says it also sets a pipe's value: {value}"
    );

    // One word for one slot, across every verb that writes it. A model that learns `value` here
    // and tries it next door must be right — and if it is ever wrong again, wrong loudly.
    let add = tool(names::ADD_INSTRUMENT_INTERFACE_INPUT);
    let add_properties = add["inputSchema"]["properties"]
        .as_object()
        .expect("input properties");
    assert!(
        add_properties.contains_key("value") && !add_properties.contains_key("default"),
        "the pipe's seed is spelled `value` wherever it is written: {add}"
    );

    // Closed, on the wire: an argument the window does not declare is refused, not dropped.
    for name in [
        names::SET_INSTRUMENT_INPUT,
        names::SET_INSTRUMENT_INTERFACE_INPUT_META,
        names::ADD_INSTRUMENT_INTERFACE_INPUT,
    ] {
        assert_eq!(
            tool(name)["inputSchema"]["additionalProperties"],
            serde_json::json!(false),
            "`{name}` must advertise a closed argument surface"
        );
    }
}

/// The keywords that make a schema say something about the value in the slot. A slot carrying none
/// of them admits anything, which is indistinguishable from admitting nothing.
const CONSTRAINTS: [&str; 7] = ["type", "enum", "const", "anyOf", "oneOf", "allOf", "$ref"];

/// One tool's schema walk: the root to resolve `$ref` against, plus the two accumulators — what the
/// walk found, and how many slots it looked at (a count with a floor, so an empty walk cannot pass).
struct SlotWalk<'a> {
    tool: &'a str,
    root: &'a serde_json::Value,
    offenders: Vec<String>,
    visited: usize,
}

impl<'a> SlotWalk<'a> {
    fn new(tool: &'a str, root: &'a serde_json::Value) -> Self {
        Self {
            tool,
            root,
            offenders: Vec::new(),
            visited: 0,
        }
    }

    fn report(&mut self, path: &str, why: &str, consequence: &str) {
        let tool = self.tool;
        self.offenders.push(format!(
            "  tool `{tool}`: property `{path}` {why}\n    ({consequence})"
        ));
    }

    /// Check one value slot and everything reachable below it. `seen` carries the `$defs` names
    /// already entered along this path, so a self-referential definition terminates.
    ///
    /// Path notation: `a.b` a property, `[]`/`[i]` an array item, `.*` a map value,
    /// `|anyOf[i]` a union branch, `~>Name` a `$defs` hop (`~>#` the self-recursive root).
    fn slot(&mut self, node: &'a serde_json::Value, path: &str, seen: &[&'a str]) {
        self.visited += 1;
        let map = match node {
            // The boolean schema forms. `false` admits nothing, which is how the closed argument
            // surface spells `additionalProperties`; `true` admits anything, which is the defect.
            serde_json::Value::Bool(false) => return,
            serde_json::Value::Bool(true) => {
                self.report(
                    path,
                    "advertises no type constraint",
                    "a schema-coercing client has nothing to coerce to and will send a string",
                );
                return;
            }
            serde_json::Value::Object(map) => map,
            other => {
                self.report(
                    path,
                    &format!("is not a schema: {other}"),
                    "a client cannot read a constraint out of it",
                );
                return;
            }
        };

        if !CONSTRAINTS.iter().any(|k| map.contains_key(*k)) {
            self.report(
                path,
                "advertises no type constraint",
                "a schema-coercing client has nothing to coerce to and will send a string",
            );
        }

        if let Some(reference) = map.get("$ref").and_then(|r| r.as_str()) {
            self.follow(reference, path, seen);
        }
        if let Some(properties) = map.get("properties").and_then(|p| p.as_object()) {
            for (name, child) in properties {
                let dot = if path.is_empty() { "" } else { "." };
                self.slot(child, &format!("{path}{dot}{name}"), seen);
            }
        }
        // Required, not optional: for a map-shaped argument schemars puts the value schema here and
        // leaves `properties` empty, so skipping it would walk straight past `inputs` and `config`.
        if let Some(values) = map.get("additionalProperties") {
            self.slot(values, &format!("{path}.*"), seen);
        }
        // Both array spellings. schemars writes a tuple as `prefixItems`, so reading only `items`
        // walks past every tuple slot there is; the draft-07 `items`-as-array form is read too
        // rather than handed to the not-a-schema arm, which would report it as a defect it is not.
        for keyword in ["items", "prefixItems"] {
            match map.get(keyword) {
                Some(serde_json::Value::Array(tuple)) => {
                    for (i, child) in tuple.iter().enumerate() {
                        self.slot(child, &format!("{path}[{i}]"), seen);
                    }
                }
                Some(single) => self.slot(single, &format!("{path}[]"), seen),
                None => {}
            }
        }
        // A union is only as constrained as its loosest branch.
        for keyword in ["anyOf", "oneOf", "allOf"] {
            if let Some(branches) = map.get(keyword).and_then(|b| b.as_array()) {
                for (i, child) in branches.iter().enumerate() {
                    self.slot(child, &format!("{path}|{keyword}[{i}]"), seen);
                }
            }
        }
    }

    /// Resolve a pointer against this tool's own root and keep walking. Skipping the hop would let
    /// a typeless leaf hide one indirection down and the whole guard pass.
    ///
    /// Two spellings resolve: `#/$defs/<name>`, and the bare `#` schemars writes for a
    /// self-recursive root type. Anything else is reported rather than followed — a pointer this
    /// walker cannot resolve is a slot it cannot vouch for.
    fn follow(&mut self, reference: &'a str, path: &str, seen: &[&'a str]) {
        let resolved = if reference == "#" {
            Some(("#", self.root))
        } else {
            reference
                .strip_prefix("#/$defs/")
                .and_then(|name| Some((name, self.root.get("$defs")?.get(name)?)))
        };
        let Some((name, target)) = resolved else {
            self.report(
                path,
                &format!("references `{reference}`, which its own schema does not define"),
                "a client cannot resolve it, so the slot constrains nothing it can read",
            );
            return;
        };
        if seen.contains(&name) {
            return;
        }
        let mut seen = seen.to_vec();
        seen.push(name);
        self.slot(target, &format!("{path}~>{name}"), &seen);
    }
}

/// The offender paths a walk collected, in walk order — the path is the message's second
/// backtick-delimited run.
fn offender_paths(walk: &SlotWalk<'_>) -> Vec<String> {
    walk.offenders
        .iter()
        .filter_map(|o| o.split('`').nth(3).map(str::to_string))
        .collect()
}

#[test]
fn every_advertised_property_constrains_its_value() {
    // A property typed `serde_json::Value` renders as a schema with no keywords at all. A client
    // that coerces arguments against the advertised schema then has nothing to coerce to and sends
    // a number as a string, which the engine correctly refuses — a whole verb dead on the wire while
    // every hand-built request in the test suite stays green. So this reads the schemas as a client
    // does. see rules: agent-mcp

    // Teeth first: one planted schema carrying a typeless leaf behind every descent the walker
    // claims to make, so no arm of it can be deleted with the suite still green. Finding these and
    // nothing else also pins the passing cases — a constrained property, a tuple slot that does
    // carry a type, and `additionalProperties: false`, which is how most of the roster spells its
    // closed argument surface and would otherwise make the guard red everywhere.
    let planted = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "$defs": {
            "Hop": {
                "type": "object",
                "additionalProperties": false,
                "properties": { "bare": { "description": "a leaf one indirection down" } }
            }
        },
        "properties": {
            "typed": { "type": "string" },
            "hop": { "$ref": "#/$defs/Hop" },
            "dangling": { "$ref": "#/$defs/Nope" },
            "list": { "type": "array", "items": true },
            "legacy": { "type": "array", "items": [{ "type": "string" }, {}] },
            "pair": { "type": "array", "prefixItems": [{ "type": "string" }, {}] },
            "malformed": { "type": "array", "items": 7 },
            "map": { "type": "object", "additionalProperties": true },
            "union": { "anyOf": [{ "type": "string" }, {}] }
        }
    });
    let mut teeth = SlotWalk::new("planted", &planted);
    teeth.slot(&planted, "", &[]);
    assert_eq!(
        offender_paths(&teeth),
        [
            "dangling",
            "hop~>Hop.bare",
            "legacy[1]",
            "list[]",
            "malformed[]",
            "map.*",
            "pair[1]",
            "union|anyOf[1]"
        ],
        "the walker must reach a typeless value through a `$defs` hop, both array spellings, a \
         tuple slot, a map value and a union branch, refuse a pointer it cannot resolve and a \
         non-schema, and report nothing else:\n{}",
        teeth.offenders.join("\n")
    );

    // schemars spells a self-recursive root as a bare `#`, which resolves and must neither be
    // reported as dangling nor walked forever.
    let recursive = serde_json::json!({
        "type": "object",
        "properties": {
            "child": { "$ref": "#" },
            "leaf": { "description": "a leaf below the recursion" }
        }
    });
    let mut root_ref = SlotWalk::new("planted", &recursive);
    root_ref.slot(&recursive, "", &[]);
    assert_eq!(
        offender_paths(&root_ref),
        ["child~>#.leaf", "leaf"],
        "a self-recursive root resolves, terminates, and still reports what is under it:\n{}",
        root_ref.offenders.join("\n")
    );

    let out = drive(&[TOOLS_LIST]);
    let response = response_with_id(&out, 2);
    let tools = response["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list result missing a tools array:\n{response}"));
    let schema_of = |name: &str| {
        &tools
            .iter()
            .find(|t| t["name"] == serde_json::json!(name))
            .unwrap_or_else(|| panic!("tools/list missing `{name}`"))["inputSchema"]
    };

    let mut offenders = Vec::new();
    let mut visited = 0;
    for tool in tools {
        let name = tool["name"].as_str().expect("a named tool");
        let schema = &tool["inputSchema"];
        let mut walk = SlotWalk::new(name, schema);
        walk.slot(schema, "", &[]);
        offenders.extend(walk.offenders);
        visited += walk.visited;
    }

    // A `tools` array that came back empty walks nothing and passes every assertion below it.
    assert!(
        visited > 100,
        "expected the whole advertised argument surface, walked {visited} value slots"
    );
    assert!(
        offenders.is_empty(),
        "every advertised property must constrain its value:\n{}",
        offenders.join("\n")
    );

    // Having a constraint is not the same as having the right one: narrowing a literal slot to
    // `string` satisfies every assertion above while restoring the exact defect this guard exists
    // to catch. So each slot's advertised forms are pinned to the forms the verb behind it accepts.
    //
    // Parity: the accepted forms are hand-written coercion matches in a crate this test may not
    // name, so neither list can be generated from the other and they are held in step here.
    let literal = serde_json::json!(["number", "string"]);
    let nullable_literal = serde_json::json!(["number", "string", "null"]);
    let wire_ref = "/properties/inputs/additionalProperties/anyOf/1";
    let pinned = [
        (
            names::SET_INSTRUMENT_INPUT,
            "/properties/value/type",
            &literal,
        ),
        (
            names::SET_INSTRUMENT_CONSTANT,
            "/properties/value/type",
            &literal,
        ),
        (
            names::ADD_INSTRUMENT_INTERFACE_INPUT,
            "/properties/value/type",
            &nullable_literal,
        ),
        (
            names::ADD_INSTRUMENT_NODE,
            "/properties/config/additionalProperties/type",
            &literal,
        ),
        (
            names::ADD_INSTRUMENT_NODE,
            "/properties/inputs/additionalProperties/anyOf/0/type",
            &literal,
        ),
        (
            names::SEND_LIVE_CONTROLS,
            "/$defs/ControlSendMessage/properties/args/items/type",
            &literal,
        ),
    ];
    for (tool, pointer, expected) in pinned {
        assert_eq!(
            schema_of(tool).pointer(pointer),
            Some(expected),
            "`{tool}` advertises the forms the verb accepts at `{pointer}`: {}",
            schema_of(tool)
        );
    }
    // The one-shot add is the only path that also takes a wire-ref, and it is advertised only there.
    let inputs = schema_of(names::ADD_INSTRUMENT_NODE);
    assert_eq!(
        inputs.pointer(&format!("{wire_ref}/properties/from/type")),
        Some(&serde_json::json!("string")),
        "the one-shot add advertises the wire-ref form it accepts: {inputs}"
    );
    assert_eq!(
        inputs.pointer(&format!("{wire_ref}/required")),
        Some(&serde_json::json!(["from"])),
        "the advertised wire-ref names the key that makes it one: {inputs}"
    );
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

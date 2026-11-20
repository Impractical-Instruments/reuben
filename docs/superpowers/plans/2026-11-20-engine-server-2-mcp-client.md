# Engine as server, part 2: reuben-mcp becomes a client — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** reuben-mcp performs every instrument read and edit by asking the running engine over the structure channel. It no longer reads or writes files, and every edit an agent makes is live.

**Architecture:**
- Part 1 (PR #896) gave the server `call`, `open`, `list` and `subscribe`. This part adds the client half of those requests to `reuben_api::engine`: `Channel` methods, plus window verbs that turn the answers into `Answer`/`Refusal`.
- Two new roster verbs, `open_instrument` and `list_instruments`, cover what `swap_instrument` did for an agent. reuben-mcp's tools become pass-throughs to those window functions, and it stops serving `swap_instrument`.
- `reuben play` gains `--headless`, `--structure-addr` (loopback only) and `--osc-in`. The eval harness, which can't open an audio device in CI, needs them to start its own server, and part 3's CLI will use them.

**Tech Stack:** Rust workspace (rmcp + tokio in reuben-mcp only), serde_json, clap; the Python eval harness in `eval/`.

**Spec:** `docs/superpowers/specs/2026-11-20-engine-as-server-design.md`. This is step 2 of its "Order of work".

**Branch:** `engine-server-mcp`, cut from `engine-as-server` (PR #896). Open its PR against `dev` once #896 has merged; until then it is stacked.

## Global Constraints

- **Clients reach servers, never files.** After this part, reuben-mcp's non-test code must not name `FsResolver`, `DocumentHistory` or `std::fs` for instrument or sample files. Its `[dependencies]` entry for `reuben-api` drops the `fs-resolver` feature; `[dev-dependencies]` may keep it.
- **One roster.** Every verb reuben-mcp serves is a `reuben_api::tools::CONTRACTS` entry, and no second list of verbs is added. `swap_instrument` stays on the roster, because reuben-web walks `CONTRACTS`; only reuben-mcp stops serving it.
- **`call` carries non-engine verbs.** Engine-kind verbs keep their own requests.
- **Default address unchanged.** It stays `127.0.0.1:9124` (`DEFAULT_STRUCTURE_ADDR`). `--structure-addr` accepts only a loopback address in this part. Non-loopback plus the token is step 6.
- **Invariants:** determinism, an RT-safe Render, an OSC-only core and the single-writer Coordinator. `--headless` must not add work to the render path.
- **Comments state mechanics only.** Never cite an issue number in a comment.
- **Gates for every task:**
  - `cargo fmt --all --check`
  - `cargo clippy --all-targets --features reuben-document/bench -- -D warnings`
  - `cargo test`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features reuben-document/bench,reuben-document/schemars`
- Task 5 also runs the eval gate: `cargo build -p reuben-mcp -p reuben-native && cd eval && python3 -m reuben_eval.gate`.

## Review Focus

1. **No `reuben play` running.** Every tool except `describe_operators` and `get_engine_status` answers `isError` at once, with `ENGINE_UNREACHABLE_GUIDANCE`. It never hangs and never falls back to a local file. Tested in Task 4.
2. **An agent edits a file that isn't the open instrument.** The server's refusal ("… is not part of the open instrument; open it to edit it") reaches the agent word for word, with `written: false`. Tested in Task 4 (native end-to-end).
3. **reuben-mcp and `reuben play` started from different directories.** A name returned by `list_instruments` works as `source` in every other verb. Tested in Task 4 (native end-to-end, with the engine rooted at another directory).
4. **`get_current_instrument` when the engine plays the built-in default rig (no source).** It answers with the hash and a projection that says it is unavailable, not an error. Tested in Task 1.
5. **The eval sidecar and a user's own `reuben play` on 9124 at the same time.** The harness uses its own free ports and never dials 9124. Tested in Task 5 (a harness test asserts the ports passed differ from the defaults).

---

### Task 1: Client half of `call` in reuben-api

**Files:**
- Modify: `crates/reuben-api/src/engine/channel.rs` (new `Channel` methods and tests)
- Modify: `crates/reuben-api/src/engine/verbs.rs` (new window verbs)

**Interfaces:**
- Consumes (from part 1, `engine/wire.rs`):
  - `Request::Call { tool, args }`, `Request::Open { name, expect }`, `Request::List { what: ListWhat }`
  - `Response::Called { output, summary }`, `Response::Refused { message }`, `Response::Listed { names }`
  - `open` answers with the same responses as `swap`: `SwapReport`, `Conflict`, `Held`, `Refused` or `Error`.
- Produces:
  - `pub enum CallOutcome { Called { output: serde_json::Value, summary: String }, Refused(String) }` in `channel.rs`, re-exported from `engine`.
  - `Channel::call(&self, tool: &str, args: serde_json::Value) -> Result<CallOutcome, ChannelError>`
  - `Channel::open(&self, name: String, expect: Option<String>) -> Result<SwapOutcome, ChannelError>`, with `Response::Refused` mapped to a new `SwapOutcome::Refused(String)` variant. `swap` maps it the same way.
  - `Channel::list(&self, what: ListWhat) -> Result<Vec<String>, ChannelError>`
  - `pub fn call_tool<A: Serialize>(tool: &str, args: &A, channel: &Channel) -> Result<Answer<serde_json::Value>, Refusal>` in `verbs.rs`. `Called` becomes `Answer`; `Refused` becomes `Refusal` with the server's message; a channel error becomes the same refusal the other engine verbs give (`refuse("…", why)`, whose unreachable case carries `ENGINE_UNREACHABLE_GUIDANCE`).
  - `pub fn current_instrument(channel: &Channel) -> Result<Answer<CurrentInstrument>, Refusal>` in `verbs.rs`. It reads `get_document` for `source` and `content_hash`, and fills `projection` from `call_tool("describe_instrument", {source, view: "index"})`, using the same rendered index text `project_installed` produces. With `source: None`, it answers `projection: "(projection unavailable: no source to describe)"`. A refused describe goes into `projection` as `"(projection unavailable: <message>)"`, matching `project_installed`'s failure form. It never returns `Err` for either case.
  - The existing `get_current_instrument` stays unchanged, for reuben-web.

- [ ] **Step 1: Write the failing tests** in `channel.rs`'s test module. Use the fake transport style already there, a scripted response line per request.
  - `call_relays_called_output_and_summary`: the server line `{"called":{"output":{"x":1},"summary":"s"}}` gives `CallOutcome::Called { output: json!({"x":1}), summary: "s" }`, and the request line written is `Request::Call { tool: "describe_instrument", args }` serialized.
  - `call_relays_a_refusal_verbatim`: a `Refused { message: "m" }` line gives `CallOutcome::Refused("m")`.
  - `open_maps_refused_held_conflict_and_report`: one case per response.
  - `list_returns_the_names_in_order`.

  Then add tests in `verbs.rs`:
  - `call_tool_unreachable_carries_the_start_reuben_play_guidance`: the refusal message contains `ENGINE_UNREACHABLE_GUIDANCE`.
  - `current_instrument_with_no_source_answers_unavailable_not_error`.
  - `current_instrument_projects_through_describe`: the fake answers `get_document` with `source: Some("a.json")`, then answers the `call` with a describe output. Assert that the second request line is a `call` to `describe_instrument` naming `a.json`, and that `projection` equals the index text from that output.
- [ ] **Step 2:** Run `cargo test -p reuben-api engine::`. Expected: the tests fail to compile, because the new methods don't exist yet.
- [ ] **Step 3:** Implement the interfaces above. Read `describe_instrument`'s output type (`authoring::DocumentView`) to find the field that holds the rendered index, and take `projection` from that field.
- [ ] **Step 4:** Run `cargo test -p reuben-api`. Expected: PASS. Then run the gates.
- [ ] **Step 5:** Commit: `Structure channel client: call, open and list, and the window verbs over them`.

### Task 2: `open_instrument` and `list_instruments` on the roster

**Files:**
- Modify: `crates/reuben-api/src/tools.rs` (two `Contract`s of kind `Engine`, their `names` consts, prose)
- Modify: `crates/reuben-api/src/engine/args.rs`, `engine/result.rs`, `engine/verbs.rs`

**Interfaces:**
- Consumes: from Task 1, `Channel::open` and `Channel::list`.
- Produces:
  - `names::OPEN_INSTRUMENT = "open_instrument"` and `names::LIST_INSTRUMENTS = "list_instruments"`.
  - `pub struct OpenInstrument { pub name: String, #[serde(default)] pub expect: Option<String> }` and `pub struct ListInstruments { pub what: ListWhat }`, both `Deserialize + Serialize + JsonSchema`, with field docs written for a model.
  - `pub fn open_instrument(args: &OpenInstrument, channel: &Channel) -> Result<Answer<SwapResult>, Refusal>`. It maps outcomes the way `swap_instrument` does, and `SwapOutcome::Refused(m)` becomes a `Refusal` carrying `m`.
  - `pub struct InstrumentList { pub names: Vec<String> }` and `pub fn list_instruments(args: &ListInstruments, channel: &Channel) -> Result<Answer<InstrumentList>, Refusal>`. The summary reads `"<n> instruments offered"` or `"<n> samples offered"`.
  - Contract descriptions:
    - `open_instrument`: "Stop playing the current instrument and load and play the one named `name`, a name `list_instruments` gave. Edits apply only to the open instrument and its nested files, so open an instrument before editing it."
    - `list_instruments`: "List the names the engine offers: `what: \"instruments\"` for what `open_instrument` accepts and a `source` may name, `\"samples\"` for what a sample resource may reference."

- [ ] **Step 1: Write the failing tests.**
  - In `tools.rs`: `open_and_list_are_engine_verbs_on_the_roster`, which asserts both names are in `CONTRACTS` with `ContractKind::Engine`.
  - In `verbs.rs`, with the fake transport:
    - `open_instrument_relays_a_refusal`
    - `open_instrument_held_reads_as_not_yet_installed`, mirroring the swap test of the same shape
    - `list_instruments_summarizes_the_count`
- [ ] **Step 2:** Run `cargo test -p reuben-api`. Expected: FAIL.
- [ ] **Step 3:** Implement. If an existing roster test enumerates every Engine verb (for example the `unserved_verbs` and prose tests), extend it rather than exempt the new names.

  reuben-mcp's router refuses to start when a non-Host roster verb has no route. So in this task, also add the two routes to `crates/reuben-mcp/src/lib.rs`, as one-line delegations to `engine::open_instrument` and `engine::list_instruments`, with `output_schema` set to `SwapResult` and `InstrumentList`.
- [ ] **Step 4:** Run `cargo test`. Expected: PASS. Then run the gates.
- [ ] **Step 5:** Commit: `Roster: open_instrument and list_instruments`.

Note for the PR: reuben-web's roster tests may need these two names marked as not served when its submodule is bumped.

### Task 3: `reuben play --headless`, `--structure-addr`, `--osc-in`

**Files:**
- Modify: `crates/reuben-native/src/cli.rs` (`EngineFlags`)
- Modify: `crates/reuben-native/src/engine.rs` (a production headless render driver)
- Modify: `crates/reuben-native/src/test_support.rs` (`start_headless` uses the production driver)
- Modify: `crates/reuben-native/src/bin/reuben.rs` (print the structure address on start)

**Interfaces:**
- Consumes: `EngineConfig { structure, osc_in, .. }` and the existing `Render::Headless` in `engine.rs`.
- Produces:
  - `EngineFlags.headless: bool` (`--headless`): no audio device; a thread renders blocks at 48 kHz with the fake's block size, output discarded. This moves the timer loop of `test_support::FakeCallback` into `engine.rs` as the driver `Render::Headless` uses, and `FakeCallback` becomes a thin wrapper over it, so tests and `--headless` run one implementation.
  - `EngineFlags.structure_addr: Option<String>` (`--structure-addr <HOST:PORT>`) sets `EngineConfig.structure`. Before start, a non-loopback IP is refused with `error: --structure-addr must be a loopback address until token authentication exists`.
  - `EngineFlags.osc_in: Option<String>` (`--osc-in <HOST:PORT>`) sets `EngineConfig.osc_in`.
  - `reuben play` prints `structure channel on <addr>` once the server is bound.

- [ ] **Step 1: Write the failing tests.**
  - In `cli.rs`:
    - `structure_addr_flag_reaches_the_config`
    - `a_non_loopback_structure_addr_is_refused` (for `0.0.0.0:9124` and `192.168.1.2:9124`)
    - `headless_and_osc_in_flags_parse`
  - In `crates/reuben-native/tests/`, a new `play_headless.rs`: spawn the built `reuben` binary (`env!("CARGO_BIN_EXE_reuben")`) with `play --headless --structure-addr 127.0.0.1:<free> --osc-in 127.0.0.1:<free> <seeded doc>`. Wait for the `structure channel on` line. Connect a `Channel` over `TcpTransport`, and assert `ping` succeeds and `get_document`'s hash equals the seeded document's. Kill the child.
- [ ] **Step 2:** Run `cargo test -p reuben-native`. Expected: FAIL.
- [ ] **Step 3:** Implement. Free ports come from binding `127.0.0.1:0` and reading the port.
- [ ] **Step 4:** Run `cargo test -p reuben-native`. Expected: PASS. Then run the gates.
- [ ] **Step 5:** Commit: `reuben play: --headless, --structure-addr (loopback) and --osc-in`.

### Task 4: reuben-mcp's tools become calls to the server

**Files:**
- Modify: `crates/reuben-mcp/src/lib.rs` (every Pure/Document route, `get_current_instrument`, router assertions, tests)
- Modify: `crates/reuben-mcp/src/engine_link.rs` and `crates/reuben-mcp/src/main.rs` (address from env)
- Modify: `crates/reuben-mcp/Cargo.toml`
- Modify: `crates/reuben-mcp/tests/native_engine.rs`

**Interfaces:**
- Consumes:
  - from Task 1, `engine::call_tool` and `engine::current_instrument`;
  - from Task 2, the two routes;
  - from Task 3, nothing (tests use `test_support::start_headless`).
- Produces:
  - Every route whose contract is `Document` kind, plus `describe_instrument` and `validate_instrument`, becomes `answered(engine::call_tool(names::X, &p, self.engine.structure()))`, keeping its typed `Parameters<…>` and its `output_schema`. `describe_operators` stays in-process: it reads no file and should answer with no engine running.
  - `get_current_instrument` becomes `answered(engine::current_instrument(self.engine.structure()))`.
  - The `swap_instrument` route is removed. Add `const NOT_SERVED: &[&str] = &[names::SWAP_INSTRUMENT];` beside `tool_names()`:
    - `tool_names()` and `stamp_window_prose`'s "a roster verb no route serves" assertion both skip `NOT_SERVED`;
    - a test asserts every `NOT_SERVED` entry is on the roster, so a stale entry fails.
  - `store`, `installed_store` and the `FsResolver` import are deleted. `reuben-api` in `[dependencies]` loses `fs-resolver`.
  - `pub const STRUCTURE_ADDR_ENV: &str = "REUBEN_STRUCTURE_ADDR";`. `main` builds `EngineLink::new(addr)` from it when set, else `EngineLink::default()`. `EngineLink::new` refuses a non-loopback address the same way Task 3's flag does (return a `Result`; `main` exits with the message).

- [ ] **Step 1: Rewrite the failing tests.**
  - In `lib.rs`, the four `document_verb_*` / `a_rejection_*` tests now drive `FakeTransport`, which gains `with_call(tool, Response)` to script `call` answers. They assert:
    - `document_verb_is_one_call_to_the_server`: `set_instrument_name` sends exactly one line, `Request::Call { tool: "set_instrument_name", args }`, whose `args` equal the tool's arguments; the `Called` output comes back as `structured_content` unchanged.
    - `document_verb_refused_by_the_server_is_iserror_with_its_message`
    - `document_verb_with_no_engine_is_iserror_with_guidance`, which covers Review Focus 1, plus `describe_operators_answers_with_no_engine`.
    - `swap_instrument_is_not_advertised` (absent from `tool_names()`) and `not_served_entries_are_on_the_roster`.
    - Delete the `swap_instrument_*` unit tests. Their behaviour lives on in reuben-api's `swap_instrument` tests.
  - In `tests/native_engine.rs`, replace the swap-based cases with `an_agent_edit_is_live`. Start `start_headless` with the instrument in a temp dir A and `instrument_root` = A. The test process's working directory is not A, which covers Review Focus 3. Build `ReubenServer::with_engine` over a TCP `EngineLink` to the engine's structure address, then:
    1. call `list_instruments {what: "instruments"}` and take the name of the seeded document;
    2. call `set_instrument_input` on it with that name;
    3. assert `written: true`, and assert `get_current_instrument`'s `content_hash` equals the edit result's `hash` with no swap call;
    4. write a second document into A and call `set_instrument_name` on its listed name, then assert `written: false` and that the message contains `open it to edit it` (Review Focus 2);
    5. call `open_instrument` on it and repeat the edit, then assert `written: true`.
  - Keep `stdio_*` tests green; update `stdio_tools_list.rs` if it lists names.
- [ ] **Step 2:** Run `cargo test -p reuben-mcp`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run `cargo test -p reuben-mcp`, then the gates, then `cargo tree -p reuben-mcp -e features --edges normal | grep fs-resolver`. Expected: tests PASS, and the grep finds nothing in normal (non-dev) edges.
- [ ] **Step 5:** Commit: `reuben-mcp: every instrument read and edit is a call to the server`.

### Task 5: Live-edit guidance and the eval harness

**Files:**
- Modify: `crates/reuben-mcp/src/lib.rs` (`INSTRUCTIONS`)
- Modify: `docs/agents/authoring.md`, and whichever lane cut `reuben-guide` produces for MCP
- Modify: `.claude/skills/patcher/SKILL.md`
- Modify: `eval/reuben_eval/mcp.py`, `eval/reuben_eval/runner.py`, `eval/reuben_eval/tasks.py`, `eval/tests/`

**Interfaces:**
- Consumes: `reuben play --headless --structure-addr --osc-in` (Task 3), `REUBEN_STRUCTURE_ADDR` (Task 4), `open_instrument` and `list_instruments` (Task 2).
- Produces: guidance where the loop is "`new_instrument` or `list_instruments` → `open_instrument` → edit (each accepted edit is live) → audition with `send_live_controls` before committing a value", and nothing tells an MCP agent to call `swap_instrument`.

- [ ] **Step 1: Prose.** In `INSTRUCTIONS`, replace the "edit … then `swap_instrument` to make it durable" loop and the engine-tool list. The new text says:
  - `reuben play` must be running for every instrument tool;
  - names come from `list_instruments`;
  - open before editing;
  - every accepted edit is live, and an edit that would break the instrument is refused with nothing written;
  - `send_live_controls` auditions are reset by the next edit.

  In `docs/agents/authoring.md`, the MCP lane's step 6 "Swap" becomes "Open" (before editing), and the swap rules of thumb say an edit swaps. `reuben-guide` slices the guide per lane, and the web lane still swaps. Edit only text the MCP lane receives, or split a shared paragraph per lane the way the guide already does elsewhere. Run reuben-guide's tests. In the patcher skill's description, replace the `swap_instrument` clause with `open_instrument`.
- [ ] **Step 2: Failing harness test.** In `eval/tests/`, add `test_session_starts_its_own_engine`. A `Session`:
  - starts `reuben play --headless` from `REPO/target/<profile>/reuben` (or `REUBEN_BIN`), with `--structure-addr 127.0.0.1:<free>`, `--osc-in 127.0.0.1:<free>`, `--instrument-root <workspace>` and cwd = workspace, and with no instrument argument, so it plays the default rig;
  - starts the sidecar with `REUBEN_STRUCTURE_ADDR` set to the same address;
  - and the ports passed are not 9124 or the default OSC-in port (Review Focus 5).

  The engine is killed when the session ends.
- [ ] **Step 3:** Implement it in `mcp.py`/`runner.py`. Update each reference solution in `tasks.py`:
  - after `new_instrument`, add an `open_instrument` step on the same `source`;
  - for a task whose seed document already exists, make `open_instrument` the first step;
  - drop any `swap_instrument` step;
  - bump `REFERENCE_REVISION` to `3`.

  Leave `test_tasks.py`'s `looks_like_file_access` name list unchanged except to add the two new verbs.
- [ ] **Step 4:** Run `cd eval && python3 -m unittest discover tests`, then `cargo build -p reuben-mcp -p reuben-native && python3 -m reuben_eval.gate`. Expected: every reference solution passes. Grounding metrics move; record the before and after numbers in the report for the PR.
- [ ] **Step 5:** Commit: `Agent guidance: every edit is live; eval sessions run their own headless engine`.

## Calls made in this plan (beyond the spec)

- **`describe_operators` stays in-process.** It reads no file, and an agent can ground itself before an engine runs.
- **Two new Engine-kind roster verbs, `open_instrument` and `list_instruments`.** Without them an agent could not hear a new instrument or learn names once `swap_instrument` is gone.
- **`new_instrument` does not open what it creates; the agent calls `open_instrument`.** The spec's "creating a top-level instrument opens it" would need the server to tell a top-level create from a nested one, and part 1 doesn't. Revisit in part 4 if the editor wants it.
- **`get_current_instrument` reads the open file's projection through `describe_instrument`.** If the files on disk are invalid while the last good tree plays, the projection describes the disk. The hash still names what plays.
- **`--structure-addr` arrives now, loopback only.** The eval harness needs its own port; step 6 widens it with the token.
- **The names `list` returns are the server's absolute paths today.** They work as names, but they show a remote client the server's directory layout. Step 6 should decide whether to make them relative to the offered roots.

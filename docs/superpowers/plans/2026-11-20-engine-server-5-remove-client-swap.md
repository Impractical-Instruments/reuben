# Engine as server, part 5: no client swap, no document by value — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A client can no longer install a document. The structure channel loses its `swap` request and `DocSource`, and `get_document` stops handing the playing document's body to a client. Editing (`call`) and opening (`open`) are the only ways a client changes what plays.

**Architecture:**
- This part is all removal; nothing new is built.
- **Order.** Task 1 moves every remaining user of `swap` onto `open` while the API still exists, so each task lands green. Task 2 removes `swap` from the wire, the server, the `Channel` and the roster. Task 3 removes `get_document`'s body and the window verb that projected it. Task 4 brings the docs in line.
- **Kept on purpose:**
  - `Response::SwapReport`, `Conflict` and `HeldSwap`: `open` answers with them.
  - `EngineHost::hold_swap` and `SwapHold`: the spec keeps held swaps for opening.
  - `Went`'s other variants.
  - The engine's internal Swap: Coordinator, mailbox, tree reload.

**Tech Stack:** Rust workspace (serde_json). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-20-engine-as-server-design.md`, step 5 of "Order of work" plus "Requests removed".

**Branch:** `engine-server-no-swap`, cut from `dev` after #900 merges (or stacked on `engine-server-cli` until then). PR against `dev`.

## Decisions this plan makes

1. **reuben-web is not a constraint** (user, 2026-11-20). The spec's "`swap_instrument` stays on the roster because reuben-web walks `CONTRACTS`" no longer applies. The `swap_instrument` roster entry, its prose (`engine_prose::SWAP_INSTRUMENT`), its args (`SwapInstrument`) and reuben-mcp's `NOT_SERVED` filter all go. Nothing here builds or checks reuben-web. For the record, its Rust side imports nothing from `reuben_api::engine`, and its JS swaps in-page.
2. **`DocSource` goes entirely, not only `DocSource::Document`.** With `Request::Swap` gone it has no user: `open` takes a name, and an outside edit reloads by origin.
3. **`get_document` keeps its name and `DocumentSnapshot` keeps its type name.** Only the `document` field goes, leaving `source` and `content_hash`, which `link.rs`, `start/mod.rs` and the editor's restart path read. The spec says to remove the body, not the request, and a rename would touch ~40 sites for no behavior.
4. **The window's `engine::get_current_instrument<S: Resources>` and `project_installed` go.** Both read the body. Part 2 kept them "for reuben-web", which never imported them. `engine::current_instrument` (projection through the server's `describe_instrument`) is what reuben-mcp serves, and it stays.
5. **An old client that sends `swap` gets the server's existing `unreadable request: …` error** on that line, and the connection keeps serving. No special-case message: every client in this repo moves in this PR.

## Global Constraints

- **Clients cannot install a document.** After Task 2, no type in `reuben_api::engine::wire` carries a document by value or by path into the server, except `Request::Open { name }`.
- **No consumer holds document bytes.** After Task 3, no structure-channel response carries a document's JSON.
- **The editor's behavior is unchanged:** follow, held opens, restart-reopen, undo, attach. Its tests that simulated "another client swapped" now simulate "another client opened" or "another client edited".
- **Invariants:** determinism, an RT-safe Render, an OSC-only core and the single-writer Coordinator. Nothing on the render path changes.
- **Comments state mechanics only.** Never cite an issue number in a comment.
- **Gates for every task:**
  - `cargo fmt --all --check`
  - `cargo clippy --all-targets --features reuben-document/bench -- -D warnings`
  - `cargo clippy -p reuben-editor --all-targets -- -D warnings`
  - `cargo test`
  - `cargo test -p reuben-editor`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features reuben-document/bench,reuben-document/schemars`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p reuben-editor`
- **Task 2** also runs the eval gate (`cargo build -p reuben-mcp -p reuben-native && cd eval && python3 -m reuben_eval.gate`). It drops a roster entry, though reuben-mcp never served it.

## Review Focus

1. **A client built before this change sends `swap`** (a stale reuben-mcp, an old editor `--attach`). It gets an `Error` response on that line and the same connection still answers `ping`: no hang, no dropped server thread. Tested in Task 2 (`a_swap_line_is_unreadable_and_the_connection_keeps_serving`, over real TCP in reuben-native).
2. **The embedded editor holds another client's `open`** exactly as it held a swap. The held-swap tests converted in Task 1 still assert `Held` and an unchanged hash, and accepting a held open installs it. Tested in Task 1 (`held_swap.rs` tests, converted).
3. **The editor restarts its engine (audio settings, input refit) while a broken document is open, or while nothing is open.** The restart reads `source` and `content_hash` only, and reopens or starts Silent as before. Covered by the existing `engine.rs` restart tests, which must still pass in Task 3 with `document` gone. Their snapshot fixtures lose the field.
4. **`get_current_instrument` over MCP while the built-in default rig plays (no source) or nothing loads.** It still answers the hash and the "projection unavailable" note, and the serialized snapshot has no `document` key. Tested in Task 3 (`a_snapshot_carries_no_document`, wire-level, plus the existing reuben-mcp test with its fixture updated).
5. **An outside edit to the open instrument after this change.** The tree reload, which used to share code with `handle_swap`, still swaps in and publishes `changed { by: outside }`. Covered by the existing `reload_after_outside_edit_swaps_and_reports_outside` in Task 2. If removing `handle_swap` moves shared code, that test is the gate.

---

### Task 1: Every user of client `swap` moves to `open`

**Files:** test modules only, plus one production string.
- Modify: `crates/reuben-api/src/engine/server.rs` (test module), `crates/reuben-api/src/engine/server/call.rs` (tests)
- Modify: `crates/reuben-native/src/structure.rs` (tests); `crates/reuben-native/tests/{structure_server,server_calls,swap_anchor}.rs`
- Modify: `crates/reuben-mcp/tests/{structure_client,native_engine}.rs`; `crates/reuben-mcp/src/lib.rs` (the fake transport's `Request::Swap` arm stays until Task 2)
- Modify: `crates/reuben-editor/src/{engine,held_swap,main,audio,ui_tests,edit}.rs` and `start/mod.rs` (tests)
- Modify: `crates/reuben-native/src/bin/reuben.rs:723` (the "structure channel unavailable" warning names `get_document/get_diagnostics/swap`; it now names what a client loses: `call/open/list/subscribe`)

**Interfaces:**
- Consumes the existing `Channel::open(name, expect) -> Result<SwapOutcome, ChannelError>`, `engine::open_instrument`, `Request::Open { name, expect }`, and `call_tool` for edits.
- Produces nothing new.

**Conversion rules:**
- **A by-path swap → `open` of the same path.** The assertions stay: `SwapReport`, `Held`, `Conflict`, the published `Opened`.
- **A swap of the same source used to show "changed"** (`Went::Swapped` publishing `Changed`) → an edit through `call` (`set_instrument_name`), which publishes `Changed { by: client }`.
- **A by-value swap.** If the test is about by-value behavior itself, delete it: `swap_anchor.rs`'s "a by-value Swap keeps the session's anchor", `structure_client.rs`'s by-value round trip, and `structure.rs:1005`. Otherwise write the document to a temp file and `open` it.
- **A test that only exists to exercise `swap`** has an `open` twin from part 1. Delete it, and name the twin in the commit message:
  - `server.rs`: the `channel.swap` round trip at :1478;
  - `call.rs`: `a_swap_publishes_opened_for_a_new_source_and_changed_for_the_same_one` and `a_refused_swap_while_nothing_plays_says_nothing_was_swapped_in`.

- [ ] **Step 1:** List every site: `grep -rnE "Request::Swap|\.swap\(|swap_instrument\(|DocSource::|SwapInstrument" crates --include=*.rs`. Classify each by the rules above in the ledger, one line per file.
- [ ] **Step 2:** Convert, crate by crate. After each crate, run its tests. Expected: PASS, with the same test count minus the deletions the ledger lists.
- [ ] **Step 3:** Run the grep again. Expected: hits only in production code (`wire.rs`, `channel.rs`, `server.rs` outside tests, `verbs.rs`, `args.rs`, `tools.rs`, `prose.rs`) and in reuben-mcp's fake-transport arm.
- [ ] **Step 4:** Run the gates. Commit: `Tests open instead of swapping: no client of this repo swaps any more`.

### Task 2: Remove client `swap` from the wire, the server, the channel and the roster

**Files:**
- Modify: `crates/reuben-api/src/engine/wire.rs`. Remove `Request::Swap` and `DocSource` and their serde tests; update the module doc's request list, which loses `swap`.
- Modify: `crates/reuben-api/src/engine/server.rs`:
  - remove `handle_swap`, `resolve_source`, `Went::Swapped` and its match arms, and the `Request::Swap` dispatch arm;
  - rewrite the module doc (lines 10–14), which describes `Request::Swap` as the install path. Installs now come from `open`, an accepted edit, and an outside reload.
- Modify: `crates/reuben-api/src/engine/channel.rs` (remove `Channel::swap` and its tests), `verbs.rs` (remove `swap_instrument` and `Verb`'s swap arm if it has one), `args.rs` (remove `SwapInstrument`), `mod.rs` (re-exports)
- Modify: `crates/reuben-api/src/tools.rs` (remove `SWAP_INSTRUMENT` and fix the doc comments at :31 and :71–72 that name it), `crates/reuben-api/src/engine/prose.rs` (remove `SWAP_INSTRUMENT`)
- Modify: `crates/reuben-mcp/src/lib.rs` (remove `NOT_SERVED` and its filter, the fake transport's `Request::Swap` arm, and `swap_instrument_is_not_advertised`; this one is now enforced by compilation, since the name no longer exists)
- Modify: `crates/reuben-native/tests/structure_server.rs` (new test)

**Interfaces:**
- Produces: `Request` without `Swap`, and no `DocSource` type. `EngineHost::hold_swap(&self, path: &str) -> bool` is unchanged and is now called only from `open`. Its doc says so.

- [ ] **Step 1: Write the failing test** in `crates/reuben-native/tests/structure_server.rs`: `a_swap_line_is_unreadable_and_the_connection_keeps_serving`.
  - Start a headless engine on a free loopback port.
  - On one raw TCP connection, write the line `{"swap":{"source":{"path":"x.json"},"expect":null}}\n`. Copy the exact spelling from the current `Request::Swap` serde test, so the line is what an old client sends.
  - Read one line. Assert it parses as `Response::Error` and its message starts with `unreadable request`.
  - On a fresh connection, `ping` answers `Pong`.
  - `get_document()`'s `content_hash` is unchanged.
- [ ] **Step 2:** Run `cargo test -p reuben-native --test structure_server a_swap_line`. Expected: FAIL, because today the line is a real swap and answers a `SwapReport` or rejection.
- [ ] **Step 3:** Remove everything listed under Files. Fix what stops compiling by deleting, not by adapting: anything left that needs `swap` is a Task 1 miss, so convert it as Task 1 says and note it in the ledger.
- [ ] **Step 4:** Run the gates, then the eval gate. Expected: PASS. The eval's advertised tool list is unchanged, because reuben-mcp never served `swap_instrument`. `grep -rn "swap_instrument\|SWAP_INSTRUMENT\|DocSource" crates` prints nothing.
- [ ] **Step 5:** Commit: `The structure channel has no swap: clients open and edit`.

### Task 3: `get_document` carries no document

**Files:**
- Modify: `crates/reuben-api/src/engine/wire.rs` (`DocumentSnapshot` loses `document`; its doc says what it answers: what plays, by name and hash)
- Modify: `crates/reuben-api/src/engine/server.rs` (where the snapshot is built: stop serializing the playing document)
- Modify: `crates/reuben-api/src/engine/verbs.rs` (remove `get_current_instrument<S>`, `project_installed`, and their tests; `current_instrument` stays)
- Modify: the snapshot fixtures in `crates/reuben-mcp/src/lib.rs` tests (`with_document`, :1945), `crates/reuben-editor` tests that read `.document` (`ui_tests.rs:299`), `crates/reuben-native/src/client.rs` tests (unchanged: they read nothing but the type)
- Modify: `crates/reuben-editor/src/session/mod.rs:707` and `audio.rs:423`, test uses of `get_current_instrument`, which move to `channel.get_document()` for the hash or to `current_instrument`

**Interfaces:**
- Produces: `pub struct DocumentSnapshot { pub content_hash: String, pub source: Option<String> }`, with serde attributes on `source` unchanged.

- [ ] **Step 1: Write the failing test** in `wire.rs`'s test module: `a_snapshot_carries_no_document`. Build a `StructureState` serving a valid instrument (use the server test helper in `server.rs`), `dispatch` a `get_document` line, and serialize the response. The JSON object under the snapshot has exactly the keys `content_hash` and `source`. Run the same check for a state serving `SILENCE` with no source: keys `content_hash` only, and `content_hash` is empty.
- [ ] **Step 2:** Run `cargo test -p reuben-api a_snapshot_carries_no_document`. Expected: FAIL, because a `document` key is present.
- [ ] **Step 3:** Remove the field and the two window verbs. Update the fixtures.
- [ ] **Step 4:** Run the gates. Expected: PASS, including reuben-mcp's `get_current_instrument_projects_the_installed_graph_and_never_returns_it` and the editor's restart tests (Review Focus 3).
- [ ] **Step 5:** Commit: `get_document answers what plays by name and hash, never its body`.

### Task 4: Docs say clients open and edit

**Files:**
- Modify: whatever `grep -rniE "swap_instrument|by value|DocSource|swap (a|the) document|get_document" docs/agents crates/*/src --include=*.md --include=*.rs` turns up in prose that describes a client swapping. Expected hits:
  - `reuben-api/src/engine/mod.rs` module doc;
  - `reuben-native/src/structure.rs` (`SwapHold`'s doc says "a client's by-path swap": it holds a client's `open`);
  - `reuben-editor/src/held_swap.rs` module doc;
  - the `Caller` doc at `server.rs:575`.
- Do not rename the engine's own Swap (Coordinator, mailbox, `PreparedSwap`): it is a project term and still true.

- [ ] **Step 1:** Make the edits.
- [ ] **Step 2:** Run the gates. Run the eval gate if `docs/agents/` changed.
- [ ] **Step 3:** Commit: `Docs: a client opens or edits; only the engine swaps`.

---

After merge: update the `engine-server-part4-decisions` memory. Part 5 is merged, and only part 6 is left (`--structure-addr` beyond loopback, the token, the remote-attach check).

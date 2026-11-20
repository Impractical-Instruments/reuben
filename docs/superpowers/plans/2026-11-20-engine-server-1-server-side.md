# Engine as server, part 1: the server side — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `reuben play` (and the editor's embedded engine) serve every instrument read and edit over the structure channel: `call`, `open`, `list`, `subscribe`, with edits that go live atomically or are refused, and outside file edits followed by the server itself.

**Architecture:** One generic `call { tool, args }` request dispatched through the `reuben_api::tools` roster against a server-owned store. The store is a `Staged` overlay: a verb's write lands in memory, the whole open tree is loaded and planned through the same overlay, and only a tree that plans is flushed to disk and committed as the swap. Changes are pushed to subscribed connections as event lines. No client changes in this part; existing requests keep working.

**Tech Stack:** Rust workspace; `reuben-api` (features `authoring`, `render`, `tcp`), `reuben-native`, `notify` (workspace dep, already used by `reuben-editor`).

**Spec:** `docs/superpowers/specs/2026-11-20-engine-as-server-design.md` — read it first. This plan is the spec's "Order of work" step 1. Steps 2–6 get their own plans once this lands.

## Global Constraints

- Invariants (CLAUDE.md): determinism · RT-safe render (`process` never allocates/locks/blocks) · OSC-only core · single-writer Coordinator. Nothing here runs on the audio thread; every new path that touches the Coordinator takes its lock.
- No new request or event carries instrument document text. `hash_instrument` (takes JSON) is **not** callable through `call`.
- Comments: mechanics only, never an issue number (CLAUDE.md "Comments").
- Use the project's exact terms: Operator, Instrument, Plan, Swap, Coordinator.
- `DEFAULT_STRUCTURE_ADDR` stays `127.0.0.1:9124`; binding and the token are part 6, not here.
- Existing requests (`swap`, `get_document`, …) and their tests are unchanged in this part.
- Gates before every commit: `cargo fmt --all --check`; `cargo clippy --all-targets --features reuben-document/bench -- -D warnings`; `cargo test`. Doc gate before the final commit: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features reuben-document/bench,reuben-document/schemars`.
- `reuben-web` (sibling repo, consumes `CONTRACTS`) must keep building and keep its tool list: do not remove or rename roster entries; it filters by `kind == Document`.

## Review Focus

1. **A refused edit leaves the disk byte-identical** and the installed hash unchanged, including when the per-file validation passed but the whole tree does not plan (a voice losing an interface pipe its parent wires). → Task 4 test `edit_that_breaks_parent_is_refused_and_writes_nothing`.
2. **The server's own write must not come back as an "outside" change** when the watcher sees it. → Task 7 test `own_write_is_not_reported_as_outside`.
3. **Two clients editing at once** serialize under the Coordinator lock; a stale `expect` from the loser is a conflict, not a lost update. → Task 4 test `stale_expect_on_call_edit_is_refused`.
4. **A subscriber that disconnects** is dropped from the bus on the next publish, without panicking or blocking other subscribers. → Task 6 test `dropped_subscriber_is_pruned`.
5. **An edit naming a file outside the open tree** is refused and writes nothing. → Task 4 test `edit_outside_tree_is_refused`.

---

### Task 1: Unadvertised roster kind for host-only verbs

**Files:**
- Modify: `crates/reuben-api/src/tools.rs` (the `ContractKind` enum and the `roster!` invocation)
- Modify: `crates/reuben-api/src/authoring/prose.rs` (one sentence per new entry)
- Test: in `crates/reuben-api/src/tools.rs` `mod tests`

**Interfaces:**
- Produces: `ContractKind::Host`, and roster names `names::READ_INSTRUMENT`, `DESCRIBE_BOUNDARY`, `COPY_INSTRUMENT`, `RESTORE_INSTRUMENT`, `SET_INSTRUMENT_LAYOUT`, `SET_INSTRUMENT_RESOURCE`, `SET_INSTRUMENT_NODE_RESOURCE`, `WIRE_INSTRUMENT_INTERFACE_OUTPUT`, `WIRE_COMPATIBILITY` with wire spellings equal to the verb function names.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn host_contracts_are_on_the_roster_and_never_advertised_as_document_or_pure() {
    let host: Vec<&str> = CONTRACTS.iter().filter(|c| c.kind == ContractKind::Host).map(|c| c.name).collect();
    assert_eq!(host, [
        "read_instrument", "describe_boundary", "wire_compatibility", "copy_instrument",
        "restore_instrument", "set_instrument_layout", "set_instrument_resource",
        "set_instrument_node_resource", "wire_instrument_interface_output",
    ]);
}
```

- [ ] **Step 2: Run it to see it fail** — `cargo test -p reuben-api host_contracts_are_on_the_roster` → FAIL, `no variant named Host`.
- [ ] **Step 3: Add `ContractKind::Host`** with a `///` saying: served by a host's `call` dispatcher, never listed as a tool by a model-facing door. Append the nine entries at the end of `roster!` in the order above. Each gets a one-sentence `authoring_prose` constant (it is not shown to models, so plain mechanics).
- [ ] **Step 4: Make every door that lists tools skip `Host`.** Find each `CONTRACTS` consumer with LSP `findReferences` (reuben-mcp's tool list and its tests, `reuben-guide`, the eval census if it reads the roster). A consumer that filters by an exact kind needs nothing; one that takes "everything but Engine" must also exclude `Host`. Run `cargo test` → PASS; `cd eval && python3 -m reuben_eval.gate` unchanged (no advertised prose changed).
- [ ] **Step 5: Commit** — `git commit -m "Roster: an unadvertised Host kind for verbs only a host dispatches"`

---

### Task 2: `Staged` store — writes held in memory until the tree plans

**Files:**
- Create: `crates/reuben-api/src/staged.rs` (module behind the `authoring` feature; `pub use` from `lib.rs`)
- Test: same file, `mod tests`

**Interfaces:**
- Produces:
  - `pub struct Staged<S>`: `Clone` (all state behind one `Arc`), `Send + Sync` when `S: Send + Sync`.
  - `pub fn Staged::new(inner: S) -> Self`
  - `impl<S: Resources> Resources for Staged<S>`: `read_text` returns a pending text for that canonical source if any, else `inner`; `write_text` records into pending (never touches `inner`); `create_text` answers `Taken` if pending or `inner` has the source; `read_samples` and `canonical` delegate.
  - `pub fn pending(&self) -> Vec<String>` — canonical sources with a pending write, sorted.
  - `pub fn commit(&self) -> Result<(), ResolveError>` — writes every pending text to `inner` in sorted source order, clears pending; on the first failure stops and returns it (pending is cleared either way).
  - `pub fn discard(&self)`
  - `pub fn record_reads(&self) -> ReadLog` / `ReadLog::finish(self) -> BTreeSet<String>` — while a `ReadLog` is alive, every canonical source served by `read_text` is recorded. Used to learn the open tree's members during a load.
  - `impl<R: Resources + ?Sized> Resources for std::sync::Arc<R>` and `… for Box<R>` (in `resources.rs`), delegating every method, so `Staged<Box<dyn Resources + Send + Sync>>` is a store.

- [ ] **Step 1: Write the failing tests** (an in-memory `Map` store in the test module):
  - `write_is_invisible_to_inner_until_commit`: write `a`; `staged.read_text("a")` is the new text; `inner.read_text("a")` is the old; after `commit()` inner has the new text and `pending()` is empty.
  - `discard_drops_pending`: write, `discard()`, `read_text` is the old text.
  - `clone_shares_pending`: write through one clone, read through another.
  - `read_log_records_canonical_sources`: with a log open, read `a` and `b`; `finish()` == `{"a","b"}`; a read after `finish` is not recorded.
  - `create_sees_pending`: `create_text("a", …)` after a pending write to `a` → `Created::Taken`.
- [ ] **Step 2: Run** — `cargo test -p reuben-api staged` → FAIL (module missing).
- [ ] **Step 3: Implement** — `Mutex<BTreeMap<String, String>>` for pending keyed by `inner.canonical(source, None)`, `Mutex<Option<BTreeSet<String>>>` for the read log. Neither lock is ever held across a call into `inner`.
- [ ] **Step 4: Run** — PASS. Gates.
- [ ] **Step 5: Commit** — `git commit -m "Staged: a store whose writes wait in memory until committed"`

---

### Task 3: `authoring::call` — one dispatcher over the roster

**Files:**
- Create: `crates/reuben-api/src/authoring/call.rs`; `pub use call::{call, CallKind, Called};` in `authoring/mod.rs`
- Test: same file

**Interfaces:**
- Consumes: `CONTRACTS`, `ContractKind::{Pure, Document, Host}` (Task 1), every verb in `authoring/verbs.rs` and `restore_instrument`.
- Produces:
  - `pub struct Called { pub output: serde_json::Value, pub summary: String, pub kind: CallKind }`
  - `pub enum CallKind { Read, Edit { source: String }, Create { source: String } }` — what the server must do with the result: nothing, stage-and-plan `source`, or allow a fresh file.
  - `pub fn call<R: Resources>(tool: &str, args: serde_json::Value, resources: &DocumentHistory<R>) -> Result<Called, Refusal>`

Classification: `new_instrument`, `copy_instrument` → `Create { source: args.source | args.to }`. Every other `Document` and `Host` verb that returns `EditResult` → `Edit { source }`. `describe_*`, `validate_instrument`, `read_instrument`, `wire_compatibility` → `Read`. An `Engine`-kind name, an unknown name, or `hash_instrument` → `Refusal` naming the tool. Arguments that fail to deserialize → `Refusal` carrying serde's message.

- [ ] **Step 1: Write the failing tests**
  - `every_non_engine_contract_dispatches`: for each `CONTRACTS` entry whose kind is not `Engine`, `call(name, json!({}), &store)` returns either `Ok` or a `Refusal` whose message does **not** contain `"unknown tool"` (empty args fail deserialization; that is fine — it proves the arm exists).
  - `engine_contract_is_refused`: `call("swap_instrument", json!({}), …)` → `Err` containing `"not served by call"`.
  - `edit_is_classified_with_its_source`: on an in-memory store holding a valid doc at `"a"`, `call("set_instrument_name", json!({"source":"a","name":"x"}), …)` → `kind == CallKind::Edit { source: "a".into() }` and `output["written"] == true`.
  - `create_is_classified`: `new_instrument` at `"b"` → `CallKind::Create { source: "b".into() }`.
- [ ] **Step 2: Run** — FAIL.
- [ ] **Step 3: Implement** with one `match tool { names::X => run::<XArgs, _>(args, |a| verb(&a, store)) , … }` arm per verb; a private generic helper deserializes args, calls the verb, and serializes `Answer.output`. The `match` is the only list of verbs; it names roster symbols, so a roster removal is a compile error.
- [ ] **Step 4: Run** — PASS. Gates.
- [ ] **Step 5: Commit** — `git commit -m "authoring::call: dispatch any non-engine roster verb by name"`

---

### Task 4: The `call` request: reads, and edits that go live or are refused

**Files:**
- Modify: `crates/reuben-api/src/engine/wire.rs` (`Request::Call`, `Response::Called`, `Response::Refused`)
- Modify: `crates/reuben-api/src/engine/server.rs` (`StructureState` holds the store; `handle_call`)
- Modify: `crates/reuben-api/src/engine/mod.rs` (`install_initial` takes the shared store)
- Modify: `crates/reuben-native/src/engine.rs` (build the store once, hand it to both)
- Test: `server.rs` `mod tests`

**Interfaces:**
- Consumes: `Staged` (Task 2), `authoring::call` / `CallKind` (Task 3), `Coordinator::prepare_document_from` / `commit_swap`.
- Produces:
  - `Request::Call { tool: String, args: serde_json::Value }` (verb `"call"`)
  - `Response::Called { output: serde_json::Value, summary: String }`, `Response::Refused { message: String }`
  - `pub type ServerStore = DocumentHistory<Staged<Box<dyn Resources + Send + Sync>>>;`
  - `StructureState::new(coordinator, host, store: Arc<ServerStore>)` — signature change; the `Staged` clone inside is the same one the Coordinator's resolver holds.
  - `install_initial(doc_json, resources: Staged<Box<dyn Resources + Send + Sync>>, config)` — the Coordinator owns this clone.
  - `StructureState::tree(&self) -> BTreeSet<String>` — canonical sources read by the last successful install (from `Staged::record_reads` around every prepare).

`handle_call` runs every call, reads included, under the Coordinator lock, so no read ever sees another call's pending write. `DocumentHistory` records a staged write even when the edit is then refused; that is accepted, because a later `restore_instrument` to such a hash goes through the same whole-tree check.

1. `Read` → answer `Called` directly. `Create` → allowed (never overwrites); `commit()` the staged create immediately; answer `Called`; no swap.
2. `Edit { source }`: if `canonical(source)` is not in `tree()` → `discard()`, answer `Called` with an `EditResult` `{ ok:false, written:false }` and one error `"<source> is not part of the open instrument; open it to edit it"`.
3. Otherwise read the top document through the store (staged text visible) and `prepare_document_from(text, top_origin)` with a `ReadLog` open. On failure → `discard()`, answer `Called` with `ok:false, written:false`, the prepare report's errors, and a note `"the edit to <source> leaves <top> unable to load; nothing was written"`. `hash` is the unchanged persisted hash.
4. On success → `commit()` the store; if that fails, drop the prepared swap and answer as step 3 with the write error. Otherwise `commit_swap`, publish render config, install probes, reclaim — exactly the tail of `handle_swap` (extract it into one private fn both call) — and store the new tree.
5. A verb `Refusal` → `Response::Refused`.

- [ ] **Step 1: Write the failing tests** (reuse the existing `Host` test double; an in-memory store with a top doc `top.json` whose `voice` node references `v.json`, and `v.json` with an interface output `out` the parent wires):
  - `read_call_answers_without_swapping`: `describe_instrument` on `top.json` → `Called`; installed hash unchanged.
  - `edit_call_goes_live`: `set_instrument_name` on `top.json` → `output.written == true`; `try_installed()` hash == `output.hash`; store's inner text changed.
  - `edit_that_breaks_parent_is_refused_and_writes_nothing`: `remove_instrument_interface_output` `out` on `v.json` → `written == false`, a note containing `"nothing was written"`; inner `v.json` byte-identical; installed hash unchanged.
  - `edit_outside_tree_is_refused`: inner also holds an unrelated valid `other.json`; `set_instrument_name` on it → `written == false`, error contains `"not part of the open instrument"`; inner unchanged.
  - `stale_expect_on_call_edit_is_refused`: two edits with the same original `expect`; the second → `written == false` (the verb's own guard), installed hash == the first's.
  - `create_call_writes_without_swapping`: `new_instrument` at `n.json` → inner has `n.json`; installed hash unchanged.
- [ ] **Step 2: Run** — `cargo test -p reuben-api --features render,tcp call_` → FAIL.
- [ ] **Step 3: Implement** as above. Update every `StructureState::new` / `install_initial` caller (LSP `findReferences`; native `engine.rs:367,435`, test helpers). In `reuben-native`, wrap the existing `FsResolver` as `Staged::new(Box::new(resolver))`, clone it into `install_initial` and into `DocumentHistory::new` for the state.
- [ ] **Step 4: Run** — PASS; full `cargo test`. Gates.
- [ ] **Step 5: Commit** — `git commit -m "Structure channel: call — reads, and edits that swap or are refused whole"`

---

### Task 5: `open` and `list`

**Files:**
- Modify: `wire.rs`, `server.rs`, `crates/reuben-native/src/structure.rs` (`NativeHost`)
- Test: `server.rs` `mod tests`; `structure.rs` tests for listing

**Interfaces:**
- Produces:
  - `Request::Open { name: String, expect: Option<String> }` → `Response::SwapReport | Held | Conflict` (same shapes as `swap`).
  - `Request::List { what: ListWhat }`, `pub enum ListWhat { Instruments, Samples }` (snake_case) → `Response::Listed { names: Vec<String> }`.
  - `EngineHost::offered(&self, what: ListWhat) -> Vec<String>` — default empty.

`open` is `handle_swap` by path with the hold offered to `EngineHost::hold_swap` (a client caller), the read going through the server store (`discard()` first so nothing pending leaks in), and the tree re-recorded on success. `NativeHost::offered`: `Instruments` → every `*.json` under the library root (`--instrument-root` / `REUBEN_INSTRUMENT_ROOT`) and under the open instrument's directory, as the absolute path the server will accept back, sorted, deduplicated; `Samples` → every `*.wav` in the same places. Nothing outside those directories.

- [ ] **Step 1: Write the failing tests**
  - `open_installs_and_records_tree`: open `other.json` → `SwapReport` ok; `tree()` contains `other.json`'s canonical source and not `top.json`'s.
  - `open_is_held_for_a_holding_host`: `Host { hold: true }` → `Response::Held`, installed hash unchanged.
  - `open_with_stale_expect_conflicts`.
  - native `offered_lists_only_library_and_open_dir` (tempdir with `lib/a.json`, `lib/s.wav`, `open/b.json`, `elsewhere/c.json`): `Instruments` == `[abs(lib/a.json), abs(open/b.json)]`; `Samples` == `[abs(lib/s.wav)]`.
- [ ] **Step 2: Run** — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — PASS. Gates.
- [ ] **Step 5: Commit** — `git commit -m "Structure channel: open and list"`

---

### Task 6: `subscribe` — changes pushed to clients

**Files:**
- Create: `crates/reuben-api/src/engine/events.rs`
- Modify: `wire.rs` (`Request::Subscribe`, `Response::Subscribed`, `Event`), `server.rs` (publish points, `InProcess::subscribe`), `tcp.rs` (client `subscribe`), `crates/reuben-native/src/structure.rs` (`handle_connection` streams after `Subscribe`)
- Test: `events.rs`, `server.rs`, `structure.rs`

**Interfaces:**
- Produces:
  - `#[serde(tag = "event", rename_all = "snake_case")] pub enum Event { Changed { hash: String, by: ChangedBy }, Opened { name: String, hash: String }, Invalid { errors: Vec<Diag> } }`, `pub enum ChangedBy { Client, Outside }` (snake_case). `Event::to_ndjson` / `from_ndjson` like `Request`.
  - `pub struct Bus` (in `events.rs`): `subscribe(&self) -> std::sync::mpsc::Receiver<Event>`, `publish(&self, event: Event)` — sends to each sender, dropping those whose receiver is gone. Held in `StructureState`.
  - `InProcess::subscribe(&self) -> Receiver<Event>`.
  - `TcpTransport::subscribe(&self) -> std::io::Result<impl Iterator<Item = std::io::Result<Event>>>` — opens its own connection, sends `Subscribe`, expects `Subscribed`, then yields one `Event` per line with no read timeout.

Publish points (after the lock work, still under it so order matches install order): a successful `call` edit → `Changed { by: Client }`; a successful `open`, or a `swap` whose source differs from the previous → `Opened`; a `swap` of the same source → `Changed { by: Client }`. Task 7 adds `Outside` and `Invalid`.

On TCP, after answering `Subscribed`, the handler loops `recv_timeout(ACCEPT_POLL)` on the receiver, writing each event as a line and checking the shutdown flag; a write error ends the connection. That connection serves no further requests.

- [ ] **Step 1: Write the failing tests**
  - `dropped_subscriber_is_pruned`: subscribe twice, drop one receiver, `publish` twice → the live one gets both; no panic; `Bus` holds one sender after the second publish.
  - `edit_publishes_changed_by_client` (InProcess): subscribe, `call` an edit → `Changed { hash: <new>, by: Client }`.
  - `refused_edit_publishes_nothing`: `try_recv()` is `Empty`.
  - `open_publishes_opened`.
  - native `tcp_subscriber_receives_events`: bind a `StructureServer` on `127.0.0.1:0`, subscribe via `TcpTransport`, `call` an edit on a second connection → the iterator yields `Changed`.
- [ ] **Step 2: Run** — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — PASS. Gates.
- [ ] **Step 5: Commit** — `git commit -m "Structure channel: subscribe, with changed/opened events"`

---

### Task 7: The server follows outside edits to its own files

**Files:**
- Modify: `server.rs` (`StructureState::reload`)
- Create: `crates/reuben-native/src/tree_watch.rs`; start it from `engine.rs` beside the structure server; add `notify.workspace = true` to `crates/reuben-native/Cargo.toml`
- Test: `server.rs`; `tree_watch.rs`

**Interfaces:**
- Produces:
  - `StructureState::reload(&self) -> Option<Event>` — under the lock: `discard()` the staged store, read the top document, prepare it with a `ReadLog`. If it fails → publish and return `Invalid { errors }` (engine keeps playing). If it plans and its hash equals the installed hash → `None`, nothing published. Otherwise commit the swap (shared tail from Task 4), record the tree, publish and return `Changed { by: Outside }`.
  - `tree_watch::spawn(state: StructureState) -> TreeWatch` (drop stops it). Watches the parent directory of every source in `state.tree()`, re-reading `tree()` after each reload so a newly referenced file is watched. Debounce: a burst is read `QUIET = 50 ms` after its last event, or `LONGEST_WAIT = 500 ms` after its first, whichever is sooner (same values and reasons as `reuben-editor/src/session/mod.rs`).

- [ ] **Step 1: Write the failing tests**
  - `reload_after_outside_edit_swaps_and_reports_outside`: change `top.json` in the inner store directly; `reload()` → `Some(Changed { by: Outside, hash })`, installed hash == `hash`.
  - `reload_of_invalid_tree_keeps_playing`: write unparseable text to `v.json` in inner; `reload()` → `Some(Invalid { .. })`; installed hash unchanged.
  - `own_write_is_not_reported_as_outside`: `call` an edit, then `reload()` → `None`.
  - native `watcher_follows_a_file_edit` (tempdir, real `FsResolver`, `InProcess` subscriber): rewrite the top file with a renamed instrument → within 2 s the subscriber gets `Changed { by: Outside }`.
- [ ] **Step 2: Run** — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — PASS. Gates.
- [ ] **Step 5: Commit** — `git commit -m "reuben play follows outside edits to its instrument's files"`

---

### Task 8: End to end over TCP, and the docs

**Files:**
- Create: `crates/reuben-native/tests/server_calls.rs`
- Modify: `crates/reuben-api/src/engine/wire.rs` (the `Request` doc comment's verb count), `crates/reuben-api/src/engine/mod.rs` module doc (the server now owns authoring for its clients), `docs/agents/` only if a page describes the structure channel's request list

- [ ] **Step 1: Write the test** — start the native engine headless the way existing `reuben-native` integration tests do, on `127.0.0.1:0`, on a tempdir copy of `instruments/` library files. Over `TcpTransport`: `list` instruments contains the open file; `call describe_instrument` answers; subscribe; `call set_instrument_name` → `written == true` and the subscriber yields `Changed { by: Client }`; the file on disk carries the new name; a breaking edit → `written == false` and the file is byte-identical; `open` a second library instrument → `Opened`.
- [ ] **Step 2: Run** — `cargo test -p reuben-native --test server_calls` → PASS.
- [ ] **Step 3: Update the docs** listed above; run the doc gate.
- [ ] **Step 4: Commit** — `git commit -m "End-to-end: a client edits and follows reuben play over TCP"`

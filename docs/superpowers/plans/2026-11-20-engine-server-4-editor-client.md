# Engine as server, part 4: reuben-editor becomes a client — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** reuben-editor reads, edits and follows the instrument only through the server, both standalone and with `--attach`, so it never reads, writes, lists or watches an instrument or sample file.

**Architecture:**
- The server gains what the editor needs and doesn't have yet. That's five additions:
  - a `subscribe` on `Channel`;
  - one call that reads a whole tree (`read_instrument_tree`);
  - a `library` request that lists top-level instruments with their name, description and tree, plus the directories a new instrument can go in;
  - a `surface` request for the open instrument's surface doc;
  - a silent start with no open instrument.
- reuben-native's `NativeHost` serves these. `RunningEngine::offer_dir` lets standalone "Open file…" add a directory to what the server offers.
- The editor is then moved over in order:
  1. writes go through `call`;
  2. the `Session` reads the tree with one `call` and follows `subscribe` events, which replaces the file watcher and the swap-by-path;
  3. the start screen and opening go through `library` and `open`;
  4. the panels and pickers go through the server.
- Standalone, the editor starts its embedded server at launch with nothing playing.

**Tech Stack:** Rust (eframe/egui in reuben-editor), serde_json, the NDJSON structure channel.

**Spec:** `docs/superpowers/specs/2026-11-20-engine-as-server-design.md`, step 4 of "Order of work". Read the "Opening a broken document" section too.

**Branch:** `engine-server-editor` (worktree `.claude/worktrees/engine-server-editor`), cut from `dev` at 8f63a212. PR against `dev`.

## Global Constraints

- **Clients reach servers, never files.** After Task 9, reuben-editor's non-test code names none of these:
  - `FsResolver`, `DocumentHistory`, `notify`;
  - `std::fs` calls on instrument, surface or sample files;
  - `canonicalize`, or `exists()`/`is_file()` on an instrument name.

  The exceptions are files the editor owns:
  - `config.rs` (its own config dir);
  - `audio.rs` (the `--io-map` file handed to the embedded engine);
  - `pick.rs` (the standalone "Open file…" dialog);
  - `held_swap.rs` (the hold callback, which runs inside the embedded server).
- **A name is the server's.** Every instrument, nested document and sample is named by the string the server gave: a `list`/`library` entry, an `Opened.name`, or a `read_instrument_tree` source. The editor may do lexical string math on a name (parent directory, relative path between two names, file stem for display), and nothing else.
- **Edits to the open instrument apply without asking.** Only an embedded editor holds, and only an `open` of a different instrument (spec, "Calls I made").
- **One roster.** `read_instrument_tree` is a `ContractKind::Host` entry in `reuben_api::tools`. It's never advertised to a model, so the eval gate and model-facing prose don't change.
- **Wire additions are additive.** `swap`, `get_document` and `DocSource::Document` stay; part 5 removes them.
- **Invariants:** determinism, an RT-safe Render, an OSC-only core and the single-writer Coordinator.
- **Comments state mechanics only.** Never cite an issue number in a comment.
- **Gates for every task:**
  - `cargo fmt --all --check`
  - `cargo clippy --all-targets --features reuben-document/bench -- -D warnings`
  - `cargo clippy -p reuben-editor --all-targets -- -D warnings`
  - `cargo test`
  - `cargo test -p reuben-editor`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features reuben-document/bench,reuben-document/schemars`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p reuben-editor`

## Calls made in this plan (beyond the spec)

- **Calls stay synchronous on the UI thread.** An edit, a tree read and an `open` are each one blocking round trip, on the budgets `Channel` already has.
  - In process (standalone) this costs what today's local load costs.
  - Over TCP it stalls a frame by one round trip.
  - Lists (`library`, `list`) and the users listing are fetched on a worker thread, as `users` is today.
  - Cost if wrong: a slow remote engine makes the editor stutter on each edit. Moving calls to a worker is a later change behind the same `edit::write` signature.
- **Every event triggers one re-read.** Each frame drains all pending events and re-reads the tree once if there were any, whatever their kind. `Changed.hash` names only the top document, so the event alone can't say which file changed.
- **Status comes from the tree.** `Status::Held(errors)` means some document of the tree doesn't read, or its report has errors. `Status::Swapping` and `swap_again` go, because an accepted edit is already live.
- **A refused restore forgets the stack.** If `restore_instrument` is refused (state evicted, or the source is no longer a document), the editor drops the undo/redo stack behind it, as `Restored::Forgotten` does today. A channel failure leaves the stack and shows a refusal.
- **An embedded-engine restart (input refit, audio settings) clears undo.** The new server's history doesn't hold the old one's states. The session clears `Undo` and shows "Undo history cleared: the engine restarted."
- **The standalone editor always runs a server.** It starts at launch with `Instrument::Silent`. If the device won't open, it starts with `start_headless` and reports the device error, so the library still lists and edits still work without sound.
- **Users-of is computed on the client from `library`.** Each entry carries `reads`, the sources its tree reads. "Who uses this document" is the set of entries other than the document itself whose `reads` contain it.
- **Recent instruments are stored as names.** An entry that the server's `list instruments` doesn't offer is greyed out. The list is per-user editor config, as today.
- **Free names for a local copy** come from `list instruments`. The editor picks the first `<stem>-copy.json`, then `<stem>-copy-2.json` and so on, in the source's directory, that isn't already listed.
- **Enforcement is a source-scan test** (Task 9). reuben-native enables `reuben-api/fs-resolver`, so with feature unification, dropping the feature from the editor's dependencies wouldn't stop the editor compiling against `FsResolver`.

## Review Focus

1. **Attached to an engine rooted in a directory the editor can't read.** Opening, editing, following outside edits and undo all work, and nothing touches the editor's disk. Tested in Task 9 (end-to-end over TCP with a temp root that is neither the cwd nor passed to the editor).
2. **A text editor changes a nested voice file while it's open.** The canvas shows the new structure within one event, and the status reflects whether the new tree loads. Tested in Task 6.
3. **Opening a broken instrument.** The editor shows the document and its errors with `Status::Held`, and nothing plays. The edit that fixes it makes the status `Current` and it plays. Tested in Task 6.
4. **A node drag that commits several `set_instrument_layout` calls.** The events these edits publish trigger at most one tree read per frame, not one per event. Tested in Task 6 (count `read_instrument_tree` calls through a counting transport).
5. **The attached engine dies mid-session.** The next edit returns a refusal within the channel budget, never a panic. The subscription ends, `Follow` reconnects once the engine is back, and the tree is re-read. Tested in Task 6.

---

### Task 1: `Channel::subscribe`, and a silent write publishes `Changed`

**Files:**
- Modify: `crates/reuben-api/src/engine/channel.rs` (`Transport`, `Channel`)
- Modify: `crates/reuben-api/src/engine/server.rs` (`impl Transport for InProcess`)
- Modify: `crates/reuben-api/src/engine/tcp.rs` (`impl Transport for TcpTransport`)
- Modify: `crates/reuben-api/src/engine/server/call.rs` (`written_while_silent`)
- Test: inline `#[cfg(test)]` modules in those files, and `crates/reuben-native/tests/server_calls.rs`

**Interfaces:**
- Produces:
  - `Transport::subscribe(&self) -> io::Result<Box<dyn Iterator<Item = io::Result<Event>> + Send>>`, a trait method whose default returns `io::ErrorKind::Unsupported`;
  - `pub type Events = Box<dyn Iterator<Item = Event> + Send>`;
  - `Channel::subscribe(&self) -> Result<Events, ChannelError>`. The iterator ends when the server closes the stream or a read fails.

- [ ] **Step 1: Write the failing tests**
  - `a_channel_subscription_over_in_process_sees_an_edit`: build an `InProcess` server (the `call.rs` test `serving()` pattern), call `channel.subscribe()`, make an edit through `channel.call(...)`, then `assert!(matches!(events.next(), Some(Event::Changed { by: ChangedBy::Client, .. })))`.
  - `a_channel_subscription_over_tcp_sees_an_edit`: the same over `TcpTransport` against `reuben play --headless` in `server_calls.rs`.
  - `a_write_while_nothing_plays_publishes_changed_then_invalid`: open a broken document, make an edit that's written but leaves it broken, and assert the two next events are `Changed { hash: "".into(), by: ChangedBy::Client }` then `Invalid { .. }`.
- [ ] **Step 2: Run them and see them fail**

  Run `cargo test -p reuben-api subscription` and `cargo test -p reuben-native --test server_calls subscription`. Expected: fail to compile (no `Channel::subscribe`), and the silent-write test fails on the missing `Changed`.
- [ ] **Step 3: Implement**
  - `InProcess::subscribe` maps its `Receiver<Event>` into the iterator.
  - `TcpTransport` reuses its existing `subscribe`.
  - `written_while_silent` publishes `Changed { hash: String::new(), by: ChangedBy::Client }` before the `Invalid` it already publishes. Update `Event::Changed`'s doc so it says an empty hash means written while nothing plays.
- [ ] **Step 4: Run the gates.** All pass.
- [ ] **Step 5: Commit** `reuben-api: Channel::subscribe; a write while nothing plays publishes changed`

### Task 2: `read_instrument_tree`, the whole open tree in one call

**Files:**
- Modify: `crates/reuben-api/src/authoring/args.rs` (`ReadInstrumentTree`)
- Modify: `crates/reuben-api/src/authoring/result.rs` (`InstrumentTree`, `TreeDocument`)
- Modify: `crates/reuben-api/src/authoring/verbs.rs` (`read_instrument_tree`)
- Modify: `crates/reuben-api/src/authoring/call.rs` (dispatch arm, `CallKind::Read`)
- Modify: `crates/reuben-api/src/authoring/prose.rs` (`READ_INSTRUMENT_TREE`)
- Modify: `crates/reuben-api/src/tools.rs` (Host entry after `READ_INSTRUMENT`)
- Test: `crates/reuben-api/src/authoring/` tests, plus one `call.rs` server test

**Interfaces:**
- Produces:
  - `pub struct ReadInstrumentTree { pub source: String }`;
  - `pub struct TreeDocument { pub source: String, pub structure: Option<InstrumentStructure>, pub error: Option<String> }`;
  - `pub struct InstrumentTree { pub documents: Vec<TreeDocument> }`, with the top document first, then breadth-first, each source once;
  - `pub fn read_instrument_tree(args: &ReadInstrumentTree, resources: &dyn Resources) -> Result<Answer<InstrumentTree>, Refusal>`;
  - `tools::names::READ_INSTRUMENT_TREE = "read_instrument_tree"`.
- `TreeDocument.source` follows `crates/reuben-editor/src/session/tree.rs` `Tree::resolve`:
  - for the top document, `args.source` as given;
  - for every other document, the referencing resource's `canonical`, from the resources a node holds in a voice or patch slot (the editor's `nested`/`DOCUMENT_SLOTS`).
- The verb never refuses for an unreadable document. An unreadable document is an entry with `error` set and nothing beneath it.
- All derive `Serialize, Deserialize, schemars::JsonSchema, Debug, Clone, PartialEq`.

- [ ] **Step 1: Write the failing tests**
  - `a_tree_lists_the_top_then_each_nested_document_once`: an instrument with two voice nodes sharing one voice file. Assert `documents.len() == 2`, that `documents[0].source` is the top, and that `documents[1].source` ends with the voice file name.
  - `a_missing_nested_document_is_an_entry_with_an_error`: assert `structure.is_none()` and `error.is_some()`.
  - In `call.rs`: `read_instrument_tree_is_a_read_and_swaps_nothing`, written like `read_call_answers_without_swapping`.
- [ ] **Step 2: Run them and see them fail.** `cargo test -p reuben-api read_instrument_tree` fails to compile.
- [ ] **Step 3: Implement** with BFS over `read_instrument`. Add the roster entry and prose sentence. Check that `resolved_names` in `engine/server/call.rs` confines `source` the way it does for `read_instrument`, since the field name is the same.
- [ ] **Step 4: Run the gates.** All pass, including the roster's names/consts table test.
- [ ] **Step 5: Commit** `reuben-api: read_instrument_tree, a whole tree in one read`

### Task 3: `library` and `surface` requests

**Files:**
- Modify: `crates/reuben-api/src/engine/wire.rs` (`Request::{Library, Surface}`, `Response::{Library, Surface}`, `Library`, `LibraryEntry`)
- Modify: `crates/reuben-api/src/engine/server.rs` (`EngineHost` methods, dispatch arms)
- Modify: `crates/reuben-api/src/engine/channel.rs` (`Channel::{library, surface}`)
- Test: `crates/reuben-api/src/engine/server/call.rs` tests (test `Host`), `channel.rs` tests

**Interfaces:**
- Produces:
  - `Request::Library` (wire `{"verb":"library"}`) is answered by `Response::Library(Library)`, flat next to the `reply` tag.
  - `pub struct Library { pub instruments: Vec<LibraryEntry>, pub dirs: Vec<String> }`.
  - `pub struct LibraryEntry { pub name: String, pub title: Option<String>, pub description: Option<String>, pub error: Option<String>, pub reads: Vec<String> }`. In it:
    - `name` is the server name to `open`;
    - `title` is the document's `instrument` name;
    - `reads` is every canonical source its tree reads, itself included (`server/call.rs` `read_tree`).
  - `Request::Surface` is answered by `Response::Surface { text: Option<String> }`: the open instrument's surface doc text, or `None`. A read failure is `Response::Error`.
  - `EngineHost::library(&self, open: Option<&str>) -> Vec<String>` (default empty) gives the instruments a start screen offers.
  - `EngineHost::creatable(&self, open: Option<&str>) -> Vec<String>` (default empty) gives the directories `Library::dirs` reports.
  - `EngineHost::surface(&self, open: &str) -> Result<Option<String>, String>` (default `Ok(None)`).
  - `Channel::library(&self) -> Result<Library, ChannelError>` and `Channel::surface(&self) -> Result<Option<String>, ChannelError>`, each on the 30 s work budget.
- The server builds each entry by reading through its store, as `handle_call` reads: under the Coordinator lock, discarding anything staged. A document that doesn't read is an entry with `error` set.

- [ ] **Step 1: Write the failing tests**
  - `library_reads_each_offered_instrument`: a test host offering a good and a broken document. Assert two entries, the good one with `title` set and `reads` containing its voice, and the broken one with `error` set.
  - `library_reports_the_hosts_dirs`.
  - `surface_with_nothing_open_is_none`.
  - `surface_answers_the_hosts_text`.
  - `library_and_surface_round_trip_ndjson`: wire spellings.
- [ ] **Step 2: Run them and see them fail** (compile errors).
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the gates.**
- [ ] **Step 5: Commit** `reuben-api: library and surface requests`

### Task 4: reuben-native serves the editor: library, surface, a silent start, offered dirs

**Files:**
- Modify: `crates/reuben-native/src/structure.rs` (`NativeHost::{library, creatable, surface}`, extra offered dirs)
- Modify: `crates/reuben-native/src/engine.rs` (`Instrument::Silent`, `RunningEngine::offer_dir`)
- Modify: `crates/reuben-api/src/engine/server.rs` (whatever a silent start needs: exposing the silence install with no open instrument)
- Test: `crates/reuben-native/src/structure.rs` tests, `crates/reuben-native/tests/server_calls.rs`

**Interfaces:**
- Produces:
  - `Instrument::Silent`: the engine starts with nothing open and nothing playing, as an open of a broken document leaves it, but with no open instrument. `get_document` answers `content_hash: ""` and `source: None`, and an `open` works.
  - `RunningEngine::offer_dir(&self, dir: &Path)`: from then on the host also offers `dir` (absolute, lexically normalized) in `list`, `within`, `library` and `creatable`.
  - `NativeHost::library` lists the `.json` files directly in each offered dir: absolute, sorted, each once.
  - `NativeHost::creatable` gives the offered dirs.
  - `NativeHost::surface(open)` looks for `surfaces/<stem>.json` in up to 3 ancestors of `open`'s directory. That is `locate` and `SEARCH_DEPTH` from `crates/reuben-editor/src/controls/surface.rs`, which moves here. It returns the file's text.
- Consumes: Task 3's `EngineHost` methods.

- [ ] **Step 1: Write the failing tests**
  - `library_lists_only_documents_directly_in_an_offered_dir`: a voice in `voices/` isn't listed.
  - `an_offered_dir_is_listed_and_within`.
  - `surface_is_found_up_to_three_ancestors_up`.
  - In `server_calls.rs`: `a_silent_start_plays_nothing_then_opens_by_name`. Start headless with `Instrument::Silent`, assert `get_document().content_hash == ""` and render peak 0, then `open`, then peak > 0.
- [ ] **Step 2: Run them and see them fail.**
- [ ] **Step 3: Implement.** Use interior mutability so `offer_dir` takes `&self`. The host is shared behind an `Arc`.
- [ ] **Step 4: Run the gates.**
- [ ] **Step 5: Commit** `reuben-native: serve the library, the surface, a silent start and offered dirs`

### Task 5: The editor writes through `call`

**Files:**
- Modify: `crates/reuben-editor/src/edit.rs` (`Verb`, `write`, `write_against`, `restore`)
- Modify: every verb call site, such as `canvas/gesture.rs`, `inspector/{mod,interface,resources,model}.rs`, `edit/local_copy.rs` and `start/mod.rs` (`create`). Find them with LSP `findReferences` on `edit::write` and `edit::write_against`.
- Modify: `crates/reuben-editor/src/session/mod.rs`: remove `histories`, `history()`, `keep_states`. `Session::refresh` still reads the disk in this task.
- Modify: `crates/reuben-editor/src/ui_tests.rs` and `session::test_support::Rig`, so that every test that writes has a server.

**Interfaces:**
- Produces:
  - `pub struct Verb { pub tool: &'static str, pub args: serde_json::Value }`, plus `Verb::of<A: Serialize>(tool: &'static str, args: &A) -> Verb`. The tool is named with `reuben_api::tools::names::*`.
  - `edit::write(session: &mut Session, channel: &dyn Fn() -> Option<Channel>, source: &str, verb: impl FnOnce(Option<String>) -> Verb) -> Result<EditResult, Refusal>`. The closure receives `expect`.
  - `edit::write_against(session, channel, source, expect: Option<String>, verb)` changes the same way.
  - `edit::restore` keeps its signature and calls `restore_instrument` through `call_tool`.
- Behaviour:
  - A `None` channel is a refusal: `no engine is running, so nothing can be edited`.
  - A `ChannelError` is a refusal carrying its message.
  - An accepted edit is decoded from the call's output as `EditResult`.
  - Undo recording and `Feedback` are unchanged.
  - Following "A refused restore forgets the stack", `Restored::Forgotten` is any `Refused` answer to `restore_instrument`, and `Restored::Refused` is a channel failure.
- Test seam: `edit::test_support::writes()` counts `call`s of edit verbs that pass through `edit::run`. The `Counted` store goes away.

- [ ] **Step 1: Make the existing write and undo tests run against a server.**
  - `Rig` (session tests) and `ui_tests::editor` start a real headless engine on the scratch instrument with `reuben_native::engine::start_headless`, `structure: None`, `osc_in: None` and `follow_files: false` (this task only). They hand it to the editor's `Engine` as the embedded engine; add an `Engine` constructor taking a `RunningEngine` if none fits.
  - Change the tests that peeked at `session.history(...)` (`canvas/gesture.rs`, `edit.rs`) to assert through the server, with `read_instrument` via `call_tool` and `get_document`.
- [ ] **Step 2: Run** `cargo test -p reuben-editor`. Expected: compile errors at every verb call site.
- [ ] **Step 3: Implement** `Verb`, the new `write`/`write_against`/`restore`, and convert each call site. Each `|store, expect| verb(&Args{.., expect}, store)` becomes `|expect| Verb::of(names::VERB, &Args{.., expect})`.
- [ ] **Step 4: Add the tests**
  - `an_edit_with_no_engine_is_refused_and_writes_nothing`.
  - `an_edit_against_a_stale_view_is_a_conflict`: a second client edits through its own `call` first.
  - `undo_of_an_evicted_state_forgets_the_stack`: a server whose history keeps 1 state.
- [ ] **Step 5: Run the gates** (editor included).
- [ ] **Step 6: Commit** `reuben-editor: every edit is a call to the server`

### Task 6: The session reads the tree from the server and follows its events

**Files:**
- Modify: `crates/reuben-editor/src/session/tree.rs`:
  - `Tree::read(top: &str, channel: &Channel) -> Result<Tree, String>` through `read_instrument_tree`;
  - `Document { source: String, read: Result<InstrumentStructure, String> }`, with no `path`;
  - `Tree::blocked` is removed.
- Modify: `crates/reuben-editor/src/session/mod.rs`:
  - `top: String`;
  - `Session::open(top: &str, channel: &dyn Fn() -> Option<Channel>) -> Session`;
  - `poll(&mut self, events: &[Event], channel) -> Poll`;
  - `Status { Current, NoEngine, Held(Vec<String>) }`.

  Remove the watcher, debounce, `install`, `reinstall`, `swapping`, `swap_again`, `take_installed` and `root`.
- Delete: `crates/reuben-editor/src/session/watch.rs`.
- Create: `crates/reuben-editor/src/follow.rs`.
- Modify:
  - `crates/reuben-editor/src/main.rs` (`follow_files`, `follow_attached`, `metered`, `view`);
  - `crates/reuben-editor/src/engine.rs`: embedded `follow_files: true`, and `the_embedded_engine_does_not_follow_outside_writes` inverted to `the_embedded_engine_follows_outside_writes`;
  - `crates/reuben-editor/src/canvas/mod.rs` (`reload` takes `&str`, with no root);
  - `crates/reuben-editor/Cargo.toml` (drop `notify`).

**Interfaces:**
- Produces:
  - `Follow::start(channel: impl Fn() -> Option<Channel> + Send + 'static, wake: impl Fn() + Send + Sync + 'static) -> Follow`. A thread holds a subscription. When the subscription ends or can't be opened, it retries every 1 s and pushes a synthetic re-read signal once it reconnects.
  - `Follow::drain(&self) -> Vec<Event>`.
  - `Poll { reloaded: bool, opened: Option<String> }`. `opened` is set when an `Opened` event names something other than `top`, and `main.rs` acts on it for an attached editor.
- Behaviour:
  - `Session::poll` re-reads the tree once if `events` is non-empty and reports `reloaded` when the tree differs.
  - Status is `Held(messages)` when any document has `read: Err` or report errors, `NoEngine` when there's no channel, and `Current` otherwise.
  - `metered()` compares `plays.source == session.top()` as strings.
- Consumes: Task 1's `Channel::subscribe`, Task 2's `read_instrument_tree`.

- [ ] **Step 1: Write the failing tests** (in `session/mod.rs`, on the `Rig` with the server's own `follow_files: true`):
  - `an_outside_write_to_a_nested_document_is_followed`: write the voice file with `std::fs` (as an outside program would), then `poll_until` the session's tree shows the change. Review Focus 2.
  - `an_outside_break_holds_and_its_fix_plays`: Review Focus 2/3.
  - `opening_a_broken_document_shows_it_held_and_the_fixing_edit_plays`: Review Focus 3.
  - `a_burst_of_events_is_one_tree_read`: three layout edits, then one `poll`, then a counting transport sees one `read_instrument_tree` call. Review Focus 4.
  - `follow_reconnects_after_the_engine_returns`: over TCP, stop and restart the engine on the same port, and assert the session re-reads. Review Focus 5.
  - `an_edit_while_the_engine_is_gone_is_refused_promptly`: Review Focus 5.
- [ ] **Step 2: Run them and see them fail.**
- [ ] **Step 3: Implement.**
  - Remove the session tests that pinned the watcher, debounce or install counting (`touches`, the `Counting` `Request::Swap` assertions), or rewrite them to the event model where the behaviour they guard still exists. Name each removed test in the report.
  - When `poll` returns `opened`, `follow_attached` opens a new `Session` on that name. The `reinstall` branch goes.
- [ ] **Step 4: Run the gates** (editor included).
- [ ] **Step 5: Commit** `reuben-editor: the session reads the server's tree and follows its events`

### Task 7: Start screen, opening and launch go through the server

**Files:**
- Modify: `crates/reuben-editor/src/start/mod.rs`, `start/library.rs`, `start/recent.rs`, `pick.rs`, `main.rs`, `engine.rs`, `held_swap.rs` (accept opens by name)
- Test: `start/mod.rs`, `start/library.rs`, `start/recent.rs` and `main.rs` tests

**Interfaces:**
- Produces:
  - `start::open(name: &str, engine: &mut Engine, ...) -> Result<Session, String>` sends `channel.open(name, None)`, then `Session::open`. A `Refused`, `Conflict` or channel error is shown on the start screen.
  - `start::library::fetch(channel: Channel, wake) -> Receiver<Result<Library, String>>` runs on a worker thread.
  - `Entry` is built from `LibraryEntry` (name, title, description, error).
  - `Recent` stores `Vec<String>`.
- Behaviour:
  - At launch, standalone, the embedded engine starts with `Instrument::Silent` and `follow_files: true`. The root is `args.instrument_root.resolve()`, falling back to `reuben_native::cli::default_library()`. If the device fails, it starts with `start_headless` and the device error goes in the engine report.
  - A path argument is made absolute, its directory passed to `offer_dir`, and then it's opened by name. Rename `no_instrument_starts_no_engine` to `no_instrument_starts_a_silent_engine` and invert it.
  - Restart (refit or audio change) starts `Instrument::Silent` on the new profile, re-offers the dirs offered before, `open`s the open name, and clears undo with the note given in "Calls made".
  - "New instrument" is a name field plus a combo of `Library::dirs`. It calls `new_instrument { source: <dir>/<name>.json, name }` and then `open`s. Remove `pick::instrument_to_create`.
  - "Open file…" appears only when the engine is embedded. It runs the `rfd` dialog, `offer_dir`s the picked file's parent, and opens by name. Attached, it's hidden.
  - The held-swap accept calls `start::open` with `HeldSwap.requested`.
  - Delete `start::library::locate`, `locate_among` and `scan`.
- Consumes: Task 3/4's `library`, `offer_dir`, `Instrument::Silent`.

- [ ] **Step 1: Write the failing tests**
  - `the_start_screen_lists_the_servers_library`: Rig rooted at a copied library. The listed names equal the top-level `.json` files.
  - `opening_by_name_plays_it`.
  - `a_name_the_server_does_not_offer_is_refused_on_the_start_screen`.
  - `a_new_instrument_is_created_in_an_offered_dir_and_opened`.
  - `creating_over_an_existing_document_is_refused_and_leaves_it`, kept and rewritten.
  - `a_recent_name_the_server_no_longer_offers_is_greyed`.
  - `open_file_offers_its_directory_then_opens_it`: calls the post-dialog half directly.
  - `a_restart_clears_undo_and_says_so`.
- [ ] **Step 2: Run them and see them fail.**
- [ ] **Step 3: Implement.** Rewrite the remaining `start/mod.rs` tests to open by name, such as `opening_with_an_engine_running_swaps_the_new_instrument_in` and `the_instrument_root_reaches_both_the_files_and_the_engine`.
- [ ] **Step 4: Run the gates.**
- [ ] **Step 5: Commit** `reuben-editor: the start screen lists and opens through the server`

### Task 8: Panels and pickers go through the server

**Files:**
- Modify:
  - `crates/reuben-editor/src/controls/mod.rs`: `load` gets `describe_boundary` through `call_tool` and the surface through `channel.surface()`;
  - `crates/reuben-editor/src/controls/surface.rs`: keep `SurfaceDoc::parse`, remove `read`/`locate`;
  - `crates/reuben-editor/src/session/users.rs`: users from `Library.instruments[].reads`, fetched on a worker;
  - `crates/reuben-editor/src/inspector/resources.rs`: the sample and instrument pickers become an in-app chooser over `channel.list(Samples | Instruments)`;
  - `crates/reuben-editor/src/edit/local_copy.rs`: free name from `list instruments`;
  - `crates/reuben-editor/src/engine.rs`: input refit reads the boundary through `call`, and `loads()` goes.
- Create: `crates/reuben-editor/src/chooser.rs`, a filterable list of server names shown as paths relative to the viewed document's directory.

**Interfaces:**
- Produces:
  - `chooser::show(ui: &mut egui::Ui, id: egui::Id, names: &[String], relative_to: &str) -> Option<String>` returns the picked server name.
  - `Controls::open(top: Option<&str>, channel: &dyn Fn() -> Option<Channel>) -> Controls`.
  - `local_copy::free_name(source: &str, offered: &[String]) -> String` is pure.
- Behaviour:
  - `relative_source` stays lexical.
  - A picked name outside what the server offers can't come up, since the chooser lists only `list` answers.

- [ ] **Step 1: Write the failing tests**
  - `the_controls_panel_reads_its_surface_from_the_server`: the surface doc sits beside the server's library, and the test asserts a relabelled control.
  - `a_broken_surface_doc_is_reported_and_the_default_panel_shown`: rewrites the existing broken-surface tests to go through the server.
  - `users_are_the_library_instruments_whose_tree_reads_it`: the three-drum-instruments case from `users.rs`, now over `library`.
  - `free_name_skips_names_the_server_lists`.
  - `the_sample_chooser_offers_the_servers_samples_relative_to_the_viewed_document`.
- [ ] **Step 2: Run them and see them fail.**
- [ ] **Step 3: Implement.**
  - Delete the `users.rs` directory-scan code and its `same_file`.
  - Delete `pick::sample` and `pick::instrument_to_open`, except where "Open file…" still uses `pick`.
  - Remove the symlink spelling test in `inspector/resources.rs` if it only exercised local `canonicalize`, and say so in the report.
- [ ] **Step 4: Run the gates.**
- [ ] **Step 5: Commit** `reuben-editor: controls, users, pickers and local copies through the server`

### Task 9: Prove it: no file access, and a remote-shaped attach

**Files:**
- Create: `crates/reuben-editor/src/reaches_no_files.rs` (test-only module, `#[cfg(test)]` in `main.rs`)
- Modify: `crates/reuben-editor/src/session/mod.rs` tests (end-to-end) and `crates/reuben-editor/Cargo.toml` (`reuben-api` loses `fs-resolver` in `[dependencies]`, keeps it in `[dev-dependencies]` if tests need it)
- Modify: `docs/superpowers/specs/2026-11-20-engine-as-server-design.md` (the "Where it is today" editor bullet gets a one-line "moved in part 4" note) and `README.md`, only if a sentence there says the editor reads files itself

**Interfaces:** none new.

- [ ] **Step 1: Write the tests**
  - `the_editor_reaches_no_instrument_file`: scans `crates/reuben-editor/src/**/*.rs`, ignoring everything from the first line equal to `#[cfg(test)]` onward and any file under a test-only module. Outside the Global Constraints' exception files, it asserts that none of these tokens appear:
    - `FsResolver`, `DocumentHistory`, `notify::`;
    - `canonicalize(`, `read_dir(`, `read_to_string(`, `std::fs::write`, `.exists()`, `.is_file()`.

    On failure it names each file and line.
  - `an_attached_editor_edits_an_engine_rooted_where_it_cannot_see` (Review Focus 1):
    1. Start `reuben_native::engine::start_headless` with `structure: Some("127.0.0.1:<free>")` and `instrument_root` in a fresh temp dir. Neither the cwd nor any editor argument names that dir.
    2. `Engine::attach` to it, and `channel.library()` lists the instrument.
    3. `open` it and build a `Session`.
    4. Make an edit through `edit::write` and see the session's tree carry it.
    5. Write the file from outside and see the session follow.
    6. Undo and see the server's file return to its first hash, via `read_instrument_tree`.
- [ ] **Step 2: Run them.** Fix every finding by moving the code behind the server, not by widening the exceptions. The only exception is a file the Global Constraints already name.
- [ ] **Step 3: Run the gates.**
- [ ] **Step 4: Commit** `reuben-editor: prove the editor reaches no files and attaches without a shared disk`

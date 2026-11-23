# Surface Edit Verbs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Four `reuben-api` roster verbs that read and edit an instrument's MIDI map or on-screen
surface doc through the engine's document store, plus a MIDI map table in `reuben-editor` built on
them.

**Architecture:** Surface docs become store documents. The verbs run through
`authoring::call_scoped` over `DocumentHistory`, as instrument verbs do, and answer the same
`EditResult`. The engine's `handle_call`, the editor's write path and the editor's undo therefore
work for them unchanged, except for one branch in `handle_call`: a surface doc is committed
without a reload or Swap. The parser, the MIDI pattern matcher and the per-control checks move
from `reuben-native` and `reuben-editor` into `reuben-api`, so the engine refuses exactly what
`reuben play` and the editor would skip.

**Tech Stack:** Rust workspace, serde/serde_json/schemars (the `reuben-api` `authoring` feature),
rmcp (`reuben-mcp`), eframe/egui (`reuben-editor`).

**Spec:** `docs/superpowers/specs/2026-11-23-surface-edit-verbs-design.md`

## Global Constraints

- Branch `feat/surface-edit-verbs`; the PR targets `dev`, and its body says what it closes.
- Gates, all green before every commit that touches Rust:
  `cargo fmt --all --check` ·
  `cargo clippy --all-targets --features reuben-document/bench -- -D warnings` ·
  `cargo clippy -p reuben-editor --all-targets -- -D warnings` · `cargo test` ·
  `cargo test -p reuben-editor` ·
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features reuben-document/bench,reuben-document/schemars` ·
  `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p reuben-editor`.
- Never add `--workspace` to a gate.
- Everything new in `reuben-api` is behind the `authoring` feature. `render` stays `no_std` and
  serde-free. `reuben-core` gains nothing.
- Comments: mechanics only, never a restatement, never an issue number. Use the project's terms
  (Operator, Instrument, Swap, interface input).
- No consumer receives surface-doc text. Clients get `SurfaceView` / `EditResult`, never bytes.
- `surface_version` stays `1`. A written doc is `SurfaceDoc::to_json_pretty()` plus a trailing
  `\n`.
- Use LSP (findReferences) before every move or rename. Never Grep for a definition.

## Review Focus

1. **A surface file that doesn't parse** (hand-broken JSON, `surface_version: 2`). `describe`
   answers `SurfaceView` with `error` set and no controls. Every edit answers `written: false`
   naming the parse error, and the file is left as it was. A file that doesn't parse is never
   replaced. Test in Task 5.
2. **An instrument stem containing a dot** (`my.synth.json`) and **a stem ending `.midi`**.
   Placement must append `.json` / `.midi.json` to the stem, not use `with_extension`. Test in
   Task 3.
3. **A stale `expect`** from the editor after `reuben-mcp` edited the same map. The answer is
   `written: false`, `hash` is the current hash, and nothing is written. Test in Task 5.
4. **An edit while the open instrument doesn't load.** No boundary means `written: false` with
   "… does not load, so its MIDI map cannot be checked". A `restore_instrument` of the map still
   lands, since restores are not checked. Test in Task 6.
5. **Undo of a map edit after another instrument was opened.** The restore names a surface source
   that is not the open top instrument's, so it is refused, and the editor drops the step
   (`Restored::Forgotten`). Test in Task 6.

---

### Task 1: Surface doc types and the screen check move into `reuben-api`

**Files:**
- Create: `crates/reuben-api/src/surface/mod.rs`, `crates/reuben-api/src/surface/screen.rs`
- Modify: `crates/reuben-api/src/lib.rs` (add `#[cfg(feature = "authoring")] pub mod surface;`),
  `crates/reuben-document/src/contract.rs` (expose the hash over bytes)
- Delete: `crates/reuben-native/src/surface.rs` (its tests move to `surface/mod.rs`)
- Modify: `crates/reuben-native/src/lib.rs`, `crates/reuben-native/src/midi/routes.rs`,
  `crates/reuben-native/src/midi/watch.rs`, `crates/reuben-editor/src/controls/mod.rs`,
  `crates/reuben-editor/src/controls/model.rs` (import from `reuben_api::surface`; `resolve` calls
  `check_screen_control`)

**Interfaces:**
- Produces, in `reuben_document`: `pub fn text_hash(bytes: &[u8]) -> String`. This is the same
  FNV-1a `{:016x}` that `content_hash` uses, and `content_hash` calls it.
- Produces, in `reuben_api::surface`:
  - `enum SurfaceKind { Screen, Midi }`. Serde `rename_all = "snake_case"`, `JsonSchema`, `Copy`.
    It has `fn suffix(self) -> &'static str`, returning `""` or `".midi"`.
  - `SurfaceDoc` and `SurfaceControl`, with the fields as today plus `Serialize`, `JsonSchema`,
    `Default`, and `skip_serializing_if = "Option::is_none"` on every optional field.
  - `SurfaceDoc::parse(&str) -> Result<SurfaceDoc, SurfaceError>`, unchanged.
  - `SurfaceDoc::to_json_pretty(&self) -> String`. It puts `"surface_version": 1` first, then
    `instrument`, `cols`, `controls`.
  - `fn surface_hash(doc: &SurfaceDoc) -> String`, which is `text_hash(doc.to_json_pretty())`.
  - `fn render_surface(doc: &SurfaceDoc, surface: SurfaceKind) -> String`: the `zoom` every
    surface edit and restore answers with. One line per control: `<index> <bind> ← <midi>` for a
    MIDI map, or `<index> <bind> <widget> <label>` for a screen doc, with the range appended when
    narrowed.
  - `enum SurfaceError`, as today.
  - In `screen.rs`: `struct ScreenControl { bind: String, label: String, widget: String,
    min: Option<f64>, max: Option<f64>, note: Option<i64>, velocity: f64, degree: Option<i32>,
    group: Option<String> }`.
  - In `screen.rs`: `fn check_screen_control(control: &SurfaceControl, boundary: &Boundary) ->
    Result<(ScreenControl, Vec<String>), String>`. This is today's `resolve` logic minus building
    the editor's `Kind`. Any widget kind in the schema enum is accepted. An unknown kind is
    `Err`. The label and widget defaults, the clamped narrowing and the warnings are as today.

- [ ] **Step 1: Write the failing tests** in `surface/mod.rs` and `surface/screen.rs`.
  - Move native's five parse tests as they are.
  - `to_json_pretty_round_trips`: parse, then `to_json_pretty`, then parse again gives an equal
    doc. The text starts `{\n  "surface_version": 1`.
  - `hash_ignores_formatting`: two spellings of one doc have an equal `surface_hash`.
  - `render_surface_lists_each_control_by_index`.
  - `screen_accepts_reserved_widgets`: `xy-pad`, `grid`, `visualizer` and `keyboard` on a `value`
    input are `Ok`.
  - `screen_refuses_unknown_bind_and_missing_payloads`: an unknown bind, a `note-toggle` without
    `note`, and a `chord-button` without `degree` are each `Err` containing the bind.
  - `screen_clamps_narrowing_with_a_warning`: `min: -5` on a 0..1 input gives `min == Some(0.0)`
    and one warning.
- [ ] **Step 2: Run `cargo test -p reuben-api surface`.** Expected: FAIL to compile, because the
  module is missing.
- [ ] **Step 3: Implement.** Move the types and add `text_hash`. Write `check_screen_control`.
  Change the editor's `resolve` to call it, then map `widget` to `Kind`. The editor keeps its own
  "the editor does not draw" skip for `xy-pad`/`grid`/`visualizer`. Switch the native imports
  over.
- [ ] **Step 4: Run `cargo test` and `cargo test -p reuben-editor`.** Expected: PASS, including
  the editor's existing panel tests, unchanged.
- [ ] **Step 5: Run the gates, then commit:** `refactor(surface): surface doc types and screen check live in reuben-api`.

### Task 2: The MIDI pattern matcher and route check move into `reuben-api`

**Files:**
- Create: `crates/reuben-api/src/surface/pattern.rs` (moved from `reuben-native/src/midi/pattern.rs`)
  and `crates/reuben-api/src/surface/midi.rs`
- Modify: `crates/reuben-native/src/midi/{mod.rs,routes.rs,translate.rs,router.rs}`,
  `crates/reuben-native/tests/midi_routes.rs`

**Interfaces:**
- Consumes: Task 1's `SurfaceControl`, `SurfaceDoc`.
- Produces, in `reuben_api::surface`:
  - `OscPattern` and `PatternError`, moved with their tests.
  - `MidiKind`, with `segment()`, moved from `translate.rs`. Translate imports it.
  - `Scale`, moved from `routes.rs`.
  - `struct MidiRoute { pattern: OscPattern, kind: MidiKind, target: String, scale: Scale }`.
  - `fn check_midi_control(control: &SurfaceControl, boundary: &Boundary) ->
    Result<(MidiRoute, Vec<String>), String>`. This is today's `route` and `range`, with the
    warnings returned instead of pushed.
- `reuben-native` keeps `Route = MidiRoute` (or uses `MidiRoute` directly), `RouteTable`,
  `build_routes` (now a loop over `check_midi_control`) and `scale_value`.

- [ ] **Step 1: Move the tests.** Move `routes.rs`'s check tests that exercise single-control
  checking (kind fit, unbounded range, clamp warning, reversed class, empty `{}`) into
  `surface/midi.rs`, calling `check_midi_control`. Keep the `build_routes` and `scale_value` tests
  in native.
- [ ] **Step 2: Run `cargo test -p reuben-api surface::midi`.** Expected: FAIL to compile.
- [ ] **Step 3: Implement the move.** Use LSP findReferences on `OscPattern`, `MidiKind`, `Scale`
  and `route` first.
- [ ] **Step 4: Run `cargo test`.** Expected: PASS, including
  `crates/reuben-native/tests/midi_routes.rs`, unchanged apart from imports.
- [ ] **Step 5: Run the gates, then commit:** `refactor(midi): route checks and the pattern matcher live in reuben-api`.

### Task 3: The store says where an instrument's surface doc lives

**Files:**
- Modify: `crates/reuben-api/src/resources.rs` (the trait, plus forwarding in `Anchored`,
  `ScopedTo`, `Undecoded`, `Arc`, `Box`), `crates/reuben-api/src/staged.rs`,
  `crates/reuben-api/src/authoring/history.rs`, `crates/reuben-api/src/engine/server/call.rs`
  (`NoSamples`), `crates/reuben-api/src/fs_resolver.rs`
- Modify: `crates/reuben-native/src/engine_channel.rs` (delete `locate_surface` and
  `SURFACE_SEARCH_DEPTH`), `crates/reuben-native/src/midi/watch.rs` (use `find_surface`)

**Interfaces:**
- Produces, on `Resources`: `fn surface_source(&self, instrument: &str, surface: SurfaceKind) ->
  Option<String>`. The default is `None`. `instrument` is a canonical source, and the answer is
  canonical too: where the doc is, or where the first write will create it. Every wrapper
  forwards it as it is.
- Produces, in `reuben_api` (feature `fs-resolver`): `pub fn find_surface(instrument: &Path,
  surface: SurfaceKind) -> Option<PathBuf>`. This is today's `locate_surface`: the instrument's
  own directory and then two ancestors, 3 in all, holding `surfaces/<stem><suffix>.json`.
- `FsResolver::surface_source` answers in this order:
  1. `find_surface(kind)`.
  2. Otherwise, the directory of `find_surface(other kind)`.
  3. Otherwise, the first of the 3 searched directories that has a `surfaces/` subdirectory.
  4. Otherwise, `<instrument dir>/surfaces/`.

  The file name is always `<stem><suffix>.json`.

- [ ] **Step 1: Write the failing tests in `fs_resolver.rs`, each against a scratch dir.**
  - `finds_an_existing_doc_up_to_two_ancestors_up`: moved from native's
    `surface_is_found_in_the_own_dir_or_two_ancestors_up`.
  - `a_new_map_goes_beside_the_screen_doc`: `root/instruments/a.json` with `root/surfaces/a.json`
    gives `root/surfaces/a.midi.json`.
  - `a_new_doc_goes_in_the_nearest_existing_surfaces_dir`.
  - `a_new_doc_with_no_surfaces_dir_goes_beside_the_instrument`.
  - `a_dotted_stem_keeps_its_dots`: `my.synth.json` gives `my.synth.midi.json`.
  - `the_default_store_keeps_no_surface_docs`: a `MemoryStore` answers `None`.
  - In `staged.rs` or `history.rs`: `wrappers_forward_surface_source`, checking that
    `DocumentHistory<Staged<FsResolver>>` answers what `FsResolver` does.
- [ ] **Step 2: Run `cargo test -p reuben-api --features fs-resolver surface`.** Expected: FAIL.
- [ ] **Step 3: Implement.** Add the trait method, the forwarding, `find_surface` and placement.
  Native's watch calls `find_surface`.
- [ ] **Step 4: Run `cargo test`.** Expected: PASS.
- [ ] **Step 5: Run the gates, then commit:** `feat(store): Resources::surface_source names an instrument's surface doc`.

### Task 4: History holds and restores surface docs

**Files:**
- Modify: `crates/reuben-api/src/authoring/history.rs`, `crates/reuben-api/src/authoring/history/tests.rs`

**Interfaces:**
- Consumes: `SurfaceDoc::parse`, `to_json_pretty`, `surface_hash` (Task 1).
- `DocumentHistory::mint` tries an instrument first. If that fails, it tries
  `SurfaceDoc::parse` and answers `(surface_hash(&doc), doc.to_json_pretty())`.
- `restore_instrument_through`: when the held text parses as a `SurfaceDoc`, it restores without
  instrument validation.
  - If the current text's `surface_hash` ≠ `args.expect`, it answers `written: false`,
    `hash: <current>`, and an "expect guard" error worded as `expect_conflict`'s.
  - Otherwise it writes `prior + "\n"` and answers `written: true`, `hash: args.to`, with `zoom`
    from `render_surface` (Task 1).

- [ ] **Step 1: Write the failing tests.**
  - `a_surface_write_is_held_and_restorable`: write doc A, then doc B, through the history.
    `restore_instrument{to: hash(A), expect: hash(B)}` lands, and the file then parses equal to A.
  - `a_surface_restore_with_a_stale_expect_is_a_conflict`.
  - `a_surface_restore_is_not_checked_against_any_instrument`: the held doc binds an input
    nothing has, and it still lands.
- [ ] **Step 2: Run `cargo test -p reuben-api history`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test -p reuben-api history`.** Expected: PASS.
- [ ] **Step 5: Run the gates, then commit:** `feat(history): surface docs are held and restorable`.

### Task 5: The four verbs

**Files:**
- Modify: `crates/reuben-api/src/authoring/args.rs`, `result.rs`, `prose.rs`, `verbs.rs` (or a
  new `crates/reuben-api/src/authoring/surface_verbs.rs`, re-exported the way `verbs` is),
  `call.rs`, `crates/reuben-api/src/tools.rs`

**Interfaces:**
- Consumes: Tasks 1 to 4.
- Produces, args (all `deny_unknown_fields`, `JsonSchema`, a `///` on every field):
  - `DescribeInstrumentSurface { source: String, surface: SurfaceKind }`
  - `SetInstrumentSurfaceControl { source, surface, index: Option<usize>, control: SurfaceControl, expect: Option<String> }`
  - `RemoveInstrumentSurfaceControl { source, surface, index: usize, expect: Option<String> }`
  - `MoveInstrumentSurfaceControl { source, surface, from: usize, to: usize, expect: Option<String> }`
- Produces, result types:
  - `SurfaceView { source: String, exists: bool, hash: Option<String>, instrument:
    Option<String>, cols: Option<usize>, controls: Vec<ControlView>, warnings: Vec<String>,
    error: Option<String> }`. Here `source` is the doc's own source, which the editor uses as its
    undo step's file. `error` is set when the file doesn't parse.
  - `ControlView { control: SurfaceControl, skipped: Option<String>, warnings: Vec<String> }`
- Produces functions:
  - `fn describe_instrument_surface(args, res) -> Result<Answer<SurfaceView>, Refusal>`
  - `fn set_instrument_surface_control(args, res) -> Result<Answer<EditResult>, Refusal>`, and
    the same shape for remove and move.
- Roster: four `Document` entries after `SET_INSTRUMENT_LAYOUT`. Their symbols are
  `DESCRIBE_INSTRUMENT_SURFACE`, `SET_INSTRUMENT_SURFACE_CONTROL`,
  `REMOVE_INSTRUMENT_SURFACE_CONTROL` and `MOVE_INSTRUMENT_SURFACE_CONTROL`. The prose consts in
  `prose.rs` say what the four do and which keys each doc reads: a MIDI map reads `bind`, `midi`,
  `min`, `max`.
- `call_scoped`: describe is `read`. The three edits classify as
  `CallKind::Edit { source: res.surface_source(&res.canonical(&a.source, None), a.surface).unwrap_or_default() }`.
  `named_sources` gives the four `document()`.

Behaviour every edit shares:
- `surface_source` is `None`: `Refusal` "this store keeps no surface docs".
- The boundary comes from `describe_boundary(&DescribeBoundary { source }, &Undecoded(res))`. If
  that fails, the answer is `written: false`, with the error saying the instrument does not load,
  so its doc cannot be checked.
- The file is present but doesn't parse: `written: false` with the parse error. It is not
  overwritten.
- A stale `expect`: `written: false`, with `hash` set to the current one.
- An index out of range: `Refusal` naming the index and the length.
- `set` checks the new control with `check_midi_control` or `check_screen_control`. If it's
  skipped, the answer is `written: false`, with the error's `port` set to the bind.
- `remove` and `move` never refuse a control.
- A missing doc: `set` creates it with `instrument` set to the boundary's instrument name.
  `remove` and `move` are a `Refusal` "<instrument> has no <MIDI map|surface doc>".
- A written answer: `written: true`, `hash = surface_hash`, `report.warnings` holds every
  control's warnings and skip reasons (`port` = bind), and `zoom` is `render_surface`.

- [ ] **Step 1: Write the failing tests** against a `MemoryStore` that implements
  `surface_source` as `surfaces/<stem><suffix>.json`, with one instrument whose interface has a
  an exponential `cutoff` value 20..20000, a `notes` message input, and a `mode` enum.
  - `set_appends_a_midi_control_and_creates_the_map`
  - `set_replaces_at_index`
  - `set_refuses_a_control_that_would_not_route` (`/midi/1/cc/74` → `mode`)
  - `a_stale_expect_writes_nothing`
  - `a_map_that_does_not_parse_is_never_overwritten` (Review Focus 1)
  - `remove_and_move_reorder_and_out_of_range_is_a_refusal`
  - `describe_reports_per_control_status_and_doc_warnings` (one ok, one skipped, and an
    `instrument` mismatch)
  - `describe_of_a_missing_doc_says_it_does_not_exist`
  - `screen_set_accepts_xy_pad`
  - The existing `every_non_engine_contract_dispatches` and `…_has_a_named_sources_arm` pass with
    the new entries.
- [ ] **Step 2: Run `cargo test -p reuben-api surface`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test -p reuben-api`.** Expected: PASS. This includes the roster
  symbol/spelling test and the schema tests in `schema.rs`. If the roster count is pinned
  anywhere, update it.
- [ ] **Step 5: Run the gates, then commit:** `feat(api): surface edit verbs`.

### Task 6: The engine commits surface edits without a Swap

**Files:**
- Modify: `crates/reuben-api/src/engine/server/call.rs` (`handle_call`, `unoffered_name`),
  `crates/reuben-api/src/engine/server.rs` (`EngineHost`, remove `handle_surface`),
  `crates/reuben-api/src/engine/wire.rs`, `crates/reuben-api/src/engine/channel.rs`
- Modify: `crates/reuben-native/src/engine_channel.rs`, `crates/reuben-native/src/midi/watch.rs`,
  `crates/reuben-native/src/engine.rs` (wire the watch's trigger into `NativeHost`)

**Interfaces:**
- Produces:
  - `Event::SurfaceChanged { surface: SurfaceKind, hash: String }`.
  - `EngineHost::surface_written(&self, surface: SurfaceKind) {}`, which is a default no-op.
- Removes: `Request::Surface`, `Response::Surface`, `EngineHost::surface`, `Channel::surface`,
  `handle_surface`, and their tests. Run findReferences on each first. `reuben-web` is no
  constraint.
- `handle_call`:
  - Before `call_scoped`, for the four surface verbs: if `canonical(args.source)` ≠ the installed
    top's canonical source, answer `Refused` "only the open instrument's surface docs can be read
    or edited".
  - `unoffered_name` also accepts either `surface_source(top, kind)`.
  - After `call_scoped`, an `Edit { source }` equal to either `surface_source(top, kind)` goes to
    a new `commit_surface(state, output, summary, kind)`.
    - If `written` is false, it discards and relays.
    - Otherwise it runs `staged.commit()` (a failure is `written: false` with the error and the
      current surface hash), publishes `SurfaceChanged` through the same event path `Changed`
      uses, calls `host.surface_written(kind)`, and relays.
  - No reload, no plan, no Swap.
- Native:
  - `NativeHost` holds an `Arc<AtomicU64>` generation, and `surface_written(Midi)` increments it.
  - `RouteWatch`'s `Inputs` gains that generation, so it rebuilds even with `follow_files: false`.

- [ ] **Step 1: Write the failing tests** in `server/call.rs`'s test module, with its `Files`
  store implementing `surface_source`.
  - `a_map_edit_is_committed_without_a_swap` (no `Changed` event, one `SurfaceChanged`, the file
    is written, the host saw `surface_written(Midi)`)
  - `a_surface_verb_on_another_instrument_is_refused`
  - `undo_of_a_map_edit_lands_through_restore_instrument`
  - `a_map_edit_while_the_instrument_does_not_load_writes_nothing` and
    `its_restore_still_lands` (Review Focus 4)
  - `restoring_another_instruments_map_is_refused` (Review Focus 5)
  - In native's `engine_channel.rs`: `surface_written_bumps_the_route_watch_generation`. In
    `tests/midi_routes.rs`: `a_map_edit_routes_with_file_following_off`, which drives
    `set_instrument_surface_control` through the channel, then `send_midi`, and expects
    `midi_routed == 1`.
- [ ] **Step 2: Run `cargo test -p reuben-api engine` and `cargo test -p reuben-native`.**
  Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test`.** Expected: PASS.
- [ ] **Step 5: Run the gates, then commit:** `feat(engine): surface edits commit without a Swap and publish SurfaceChanged`.

### Task 7: `reuben-mcp` serves the four verbs

**Files:**
- Modify: `crates/reuben-mcp/src/lib.rs`, `crates/reuben-mcp/tests/stdio_tools_list.rs` (and any
  test that pins the tool count or names), `eval/` baselines if the gate asks for a rebaseline

**Interfaces:**
- Consumes: Task 5's arg types and `names::*`.
- Four `#[tool]` methods shaped like `set_instrument_name`: each is one
  `engine::call_tool(names::X, &p, self.engine.channel())` relayed with `answered`. The output
  schema is `edit_result_schema()` for the three edits and `SurfaceView`'s schema for describe,
  built the way the other read-tool schemas are.

- [ ] **Step 1: Write the failing test.** `surface_verbs_are_one_call_to_the_server` mirrors
  `document_verb_is_one_call_to_the_server` for `set_instrument_surface_control`. Extend the
  tools-list test's expected names.
- [ ] **Step 2: Run `cargo test -p reuben-mcp`.** Expected: FAIL. The router also refuses to
  start, because its tools no longer match the roster.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test -p reuben-mcp` and `cd eval && python3 -m reuben_eval.gate`.**
  Expected: PASS. If the gate reports grounding-token growth, record it in the PR body.
- [ ] **Step 5: Run the gates, then commit:** `feat(mcp): surface edit tools`.

### Task 8: The editor's controls panel reads through the verb

**Files:**
- Modify: `crates/reuben-editor/src/controls/mod.rs`

**Interfaces:**
- Consumes: `names::DESCRIBE_INSTRUMENT_SURFACE`, `SurfaceView`, and `Event::SurfaceChanged`.
- `load` calls `describe_instrument_surface { source: top, surface: Screen }`.
  - `exists: false` gives the generated panel.
  - `error: Some(e)` gives `unusable(e)`.
  - Otherwise it rebuilds the `SurfaceDoc` from the view (`instrument`, `cols`, the controls) and
    calls `Panel::from_surface` as today.
- `Controls::hear` reloads on `SurfaceChanged { surface: Screen, .. }`.

- [ ] **Step 1: Write the failing test.**
  `a_screen_surface_change_reloads_the_panel`: an event to `hear` marks the panel for reload, as
  the existing `Opened` tests check.
- [ ] **Step 2: Run `cargo test -p reuben-editor controls`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test -p reuben-editor`.** Expected: PASS.
- [ ] **Step 5: Run the editor gates, then commit:** `feat(editor): controls panel reads the surface through describe_instrument_surface`.

### Task 9: The editor's MIDI map table

**Files:**
- Create: `crates/reuben-editor/src/midi_map/mod.rs`, `model.rs`, `view.rs`
- Modify: `crates/reuben-editor/src/main.rs` (own a `MidiMap`, show it, feed it events),
  `crates/reuben-editor/src/edit.rs`

**Interfaces:**
- Produces, in `edit.rs`: `pub fn write_to(session: &mut Session, channel: &dyn Fn() ->
  Option<Channel>, source: &str, expect: Option<String>, verb: impl FnOnce(Option<String>) ->
  Verb) -> Result<EditResult, Refusal>`. It is today's private `run`, made public. It records the
  undo step for a file outside the session's tree.
- Produces, in `midi_map::model`:
  - `struct Row { index: usize, bind: String, pattern: String, min: Option<f64>, max:
    Option<f64>, status: Status }`
  - `enum Status { Ok, Warned(Vec<String>), Skipped(String) }`
  - `struct MidiMapModel { view: Option<SurfaceView>, rows: Vec<Row>, inputs: Vec<String>, error:
    Option<String> }`
  - `MidiMapModel::from_view(view: SurfaceView, boundary: &Boundary) -> Self`
  - `enum Action { Set { index: Option<usize>, control: SurfaceControl }, Remove(usize), Move {
    from: usize, to: usize } }`
  - `fn verb(top: &str, action: &Action) -> impl FnOnce(Option<String>) -> Verb`, which builds
    the matching `Verb::of(names::…, &args)` with the given `expect`.
- `midi_map::MidiMap`:
  - `open(top, channel)` reads `describe_instrument_surface(Midi)` and `describe_boundary`.
  - `hear(event)` re-reads on `SurfaceChanged { surface: Midi }`, `Changed` and `Opened`, since
    the inputs may have changed.
  - `show(ui) -> Option<Action>`.
  - `main.rs` sends an action through `edit::write_to(session, channel, &view.source,
    view.hash.clone(), verb(top, &action))`, then re-reads.
- The view is a table as sketched in the spec. Its columns are #, Input (a ComboBox over
  `inputs`), Pattern (a TextEdit committed on Enter or focus loss), Min and Max (optional
  DragValue or empty), Status, and ↑ ↓ ✕. It ends with `+ add`, which appends `{bind: first
  input, midi: "/midi/1/cc/1"}`. A refused edit's error shows on its row until the next edit.
  Where it sits: a toggle in the controls panel's header opens it as a right side panel. Follow
  the existing panels' styling (`theme.rs`).

- [ ] **Step 1: Write the failing tests** in `midi_map/model.rs`.
  - `rows_carry_each_controls_status`
  - `an_unparsable_map_shows_its_error_and_no_rows`
  - `verb_for_set_names_the_tool_and_passes_expect`
  - `move_up_from_zero_is_no_action`
  - In `ui_tests.rs`, following its harness: `editing_a_pattern_and_pressing_enter_emits_one_set`.
  - In `edit.rs`'s tests: `write_to_records_an_undo_step_for_a_file_outside_the_tree`.
- [ ] **Step 2: Run `cargo test -p reuben-editor midi_map`.** Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run `cargo test -p reuben-editor`.** Expected: PASS. Then run the app with `/run`
  against `instruments/acid-techno.json`. Add a CC row, see `surfaces/acid-techno.midi.json`
  appear at the repo root, undo it, and see it reverted.
- [ ] **Step 5: Run the editor gates, then commit:** `feat(editor): MIDI map table`.

### Task 10: Docs say to use the verbs

**Files:**
- Modify: `docs/agents/authoring.md` (the MIDI map section), `.claude/skills/control-surface/SKILL.md`
  (both docs are edited through the verbs, not by hand; the `.tosc` projection still reads the
  file), and `surfaces/surface.schema.json` (drop "the engine never reads surface docs"; say the
  engine reads and writes them through `*_instrument_surface_*`).
- If `authoring.md` is sliced by `reuben-guide`, regenerate its cuts as that crate's README says.

- [ ] **Step 1: Make the edits.**
- [ ] **Step 2: Run `cargo test -p reuben-guide`, the doc gates, and `cd eval && python3 -m reuben_eval.gate`.** Expected: PASS.
- [ ] **Step 3: Commit:** `docs: surface docs are edited through the surface verbs`.

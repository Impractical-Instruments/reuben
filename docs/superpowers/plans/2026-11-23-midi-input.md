# MIDI Input Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `reuben play` reads MIDI from the devices the device profile names and routes it to the
played Instrument's interface inputs through a MIDI surface doc, `surfaces/<stem>.midi.json`.

**Architecture:** All new code is in `reuben-native` (plus one engine-channel verb and four
diagnostics fields in `reuben-api`). A `midir` callback decodes each MIDI message to a translated
`/midi/...` address; a `Router` matches it against a route table built from the MIDI surface doc and
the Instrument's `describe_boundary`, scales the value, and pushes `/<bind>/in [value]` onto its own
control ring. The render side and `reuben-core` are untouched.

**Tech Stack:** Rust, `midir` (default features), `serde_json`, existing control rings
(`crates/reuben-native/src/control.rs`).

**Spec:** `docs/superpowers/specs/2026-11-23-midi-input-design.md`

## Global Constraints

- `reuben-core` and the Instrument document format do not change. Nothing new runs on the render thread.
- `midir` with default features; no JACK feature. MIDI bytes are decoded by hand, no parser crate.
- Channels in addresses are 1–16. Translated values: velocity, CC, pressure `v/127`; bend −1..1 with
  14-bit 8192 → 0.0, 0 → −1.0, 16383 → +1.0; program `i32` 0..127.
- Scaling normalizes to `u` in 0..1 (CC/pressure `v/127`, bend `(b+1)/2`) and maps across the
  effective range with `map`'s rule: exponential only when the curve is exponential **and** both
  bounds are > 0, else linear. `i32` targets round to nearest. `program` and `note` are not scaled.
- Every comment follows CLAUDE.md: mechanics only, no issue numbers.
- Gates before each commit: `cargo fmt --all --check`,
  `cargo clippy --all-targets --features reuben-document/bench -- -D warnings`, `cargo test`; tasks
  touching `reuben-editor` also run `cargo clippy -p reuben-editor --all-targets -- -D warnings`.
- PRs target `dev`. PR bodies say `Part of #<issue>` (open the tracking issue first if none exists).

## Review Focus

1. **MIDI realtime and system bytes** (clock `0xF8`, active sensing `0xFE`, SysEx) arrive constantly
   from many keyboards: they must be ignored without counting as "unsupported" and without logging
   under `--log-osc`. Pinned in Task 2.
2. **A MIDI map edited into invalid JSON while playing**: the previous route table keeps working
   (as an Instrument that stops loading keeps playing), with a warning. Pinned in Task 7.
3. **Exponential curve over a range touching or crossing zero** falls back to linear, never NaN.
   Pinned in Task 4.
4. **A note-on for a note already held** (retrigger, or two devices on one channel) is tracked once;
   one note-off releases it. Pinned in Task 5.
5. **A device open with no MIDI map** for the played Instrument: messages count as unmatched,
   nothing is sent, nothing panics. Pinned in Task 5.

---

### Task 1: Surface-doc parser shared by `play` and the editor, with `midi`

**Files:**
- Create: `crates/reuben-native/src/surface.rs` (moved from `crates/reuben-editor/src/controls/surface.rs`)
- Modify: `crates/reuben-native/src/lib.rs` (add `pub mod surface;`)
- Delete: `crates/reuben-editor/src/controls/surface.rs`; modify `crates/reuben-editor/src/controls/mod.rs` (`mod surface;` → `use reuben_native::surface;`)
- Modify: `surfaces/surface.schema.json` (control gains optional `"midi": {"type": "string"}`)
- Modify: `crates/reuben-native/src/engine_channel.rs` (`locate_surface` gains a suffix parameter)

**Interfaces:**
- Produces: `reuben_native::surface::{SurfaceDoc, SurfaceControl, SurfaceError}` unchanged in shape, plus `SurfaceControl::midi: Option<String>`.
- Produces: `pub(crate) fn locate_surface(instrument: &Path, suffix: &str) -> Option<PathBuf>`; the existing call passes `""`, MIDI passes `".midi"` (file name `format!("{stem}{suffix}.json")`).

- [ ] **Step 1: Move the file and its tests unchanged; point the editor at it.** Run `cargo test -p reuben-native surface` and `cargo test -p reuben-editor` — PASS (pure move).
- [ ] **Step 2: Write failing tests** in `surface.rs`: `parses_a_midi_control` — `{"surface_version":1,"instrument":"x","controls":[{"bind":"cutoff","midi":"/midi/1/cc/74","min":200,"max":8000}]}` parses with `controls[0].midi == Some("/midi/1/cc/74")`, `min == Some(200.0)`. In `engine_channel.rs`: `midi_surface_is_found_beside_the_on_screen_one` — with `surfaces/a.json` and `surfaces/a.midi.json` both present, `locate_surface(a, ".midi")` returns the second and `locate_surface(a, "")` the first.
- [ ] **Step 3: Run, verify FAIL** (`midi` field / `suffix` parameter missing).
- [ ] **Step 4: Add the field and the parameter; add `midi` to the schema's control properties.**
- [ ] **Step 5: Run `cargo test -p reuben-native` and `cargo test -p reuben-editor` — PASS.** Also check every file in `surfaces/*.json` still validates against the schema if a schema test exists (`grep -rn surface.schema.json crates`).
- [ ] **Step 6: Commit** `feat(surface): one surface-doc parser for play and the editor; controls take midi`.

### Task 2: Translate MIDI bytes to `/midi/...`

**Files:**
- Create: `crates/reuben-native/src/midi/mod.rs`, `crates/reuben-native/src/midi/translate.rs`
- Modify: `crates/reuben-native/src/lib.rs` (`pub mod midi;`)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum MidiKind { Note, Cc, Bend, Pressure, Program }
  #[derive(Debug, Clone, PartialEq)]
  pub struct Translated { pub kind: MidiKind, pub channel: u8 /* 1..=16 */, pub address: String, pub args: Vec<Arg> }
  pub enum Decoded { Message(Translated), Unsupported, Ignored }
  pub fn translate(bytes: &[u8]) -> Decoded;
  pub fn parse_translated(address: &str, args: &[Arg]) -> Option<Translated>; // for injection: validates an address+args pair as a translator would produce it
  ```
  `Arg` is `reuben_api`'s re-export of the control arg type that `OscIn::args` uses.

- [ ] **Step 1: Write failing tests** in `translate.rs`, one per row of the spec's table:
  `[0x90,60,100]` → `/midi/1/note [F32(60.0), F32(100/127)]`; `[0x80,60,64]` and `[0x90,60,0]` → `/midi/1/note [F32(60.0), F32(0.0)]`;
  `[0xB4,74,127]` → `/midi/5/cc/74 [F32(1.0)]`; `[0xE0,0x00,0x40]` → `/midi/1/bend [F32(0.0)]`, `[0xE0,0,0]` → `-1.0`, `[0xE0,0x7F,0x7F]` → `1.0`;
  `[0xD1,64]` → `/midi/2/pressure [F32(64/127)]`; `[0xCF,5]` → `/midi/16/program [I32(5)]`.
  `translate_ignores_realtime_and_system` — `[0xF8]`, `[0xFE]`, `[0xF0,1,2,0xF7]` → `Decoded::Ignored`.
  `poly_aftertouch_is_unsupported` — `[0xA0,60,10]` → `Decoded::Unsupported`. Truncated `[0x90,60]` → `Unsupported`.
  `parse_translated_round_trips` — for each case above, `parse_translated(&t.address, &t.args) == Some(t)`; `parse_translated("/midi/17/note", …)`, `"/midi/1/cc/128"`, `"/midi/1/bend" [F32(2.0)]` → `None`.
- [ ] **Step 2: Run** `cargo test -p reuben-native midi::translate` — FAIL (module missing).
- [ ] **Step 3: Implement.** Status high nibble picks the kind, low nibble + 1 the channel; bytes ≥ `0xF0` are `Ignored`.
- [ ] **Step 4: Run — PASS.**
- [ ] **Step 5: Commit** `feat(midi): translate MIDI bytes to /midi/<ch>/… addresses`.

### Task 3: OSC 1.0 address patterns

**Files:**
- Create: `crates/reuben-native/src/midi/pattern.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct OscPattern { /* compiled segments */ }
  #[derive(Debug, PartialEq)] pub enum PatternError { Malformed(String), NotMidi, MixedKinds, NoKind }
  impl OscPattern {
      pub fn parse(pattern: &str) -> Result<Self, PatternError>; // OSC 1.0: `*` `?` `[a-z]` `[!..]` `{a,b}`, per `/`-segment
      pub fn matches(&self, address: &str) -> bool;
      /// The one MidiKind this pattern can match: segment 3 tested against "note","cc","bend","pressure","program".
      pub fn midi_kind(&self) -> Result<MidiKind, PatternError>;
  }
  ```

- [ ] **Step 1: Write failing tests:** `/midi/{1,2,5,9}/cc/*` matches `/midi/5/cc/74`, not `/midi/3/cc/74`; `/midi/[4-8]/note` matches `/midi/6/note`, not `/midi/9/note`, not `/midi/16/note`; `/midi/1?/note` matches `/midi/12/note`; `[!1]` negation; `*` never crosses `/`. `midi_kind`: `/midi/1/cc/74` → `Cc`; `/midi/1/*` → `MixedKinds`; `/midi/1/{note,cc}` → `MixedKinds`; `/midi/1/no*` → `Note`; `/fader1` → `NotMidi`; `/midi/1/clock` → `NoKind`. `parse` of `/midi/[1-/note` and `midi/1/note` → `Malformed`.
- [ ] **Step 2: Run** `cargo test -p reuben-native midi::pattern` — FAIL.
- [ ] **Step 3: Implement** a segment-wise matcher (no regex crate). `midi_kind` requires segment 1 to match exactly `midi` and segment 3 to match exactly one kind name.
- [ ] **Step 4: Run — PASS.**
- [ ] **Step 5: Commit** `feat(midi): OSC 1.0 address-pattern matcher`.

### Task 4: Route table and scaling

**Files:**
- Create: `crates/reuben-native/src/midi/routes.rs`

**Interfaces:**
- Consumes: `SurfaceDoc` (Task 1), `OscPattern`, `MidiKind` (Tasks 2–3), `reuben_api::authoring::Boundary` / `PortInfo` (`kind` is `"value"`, `"signal"`, `"int"`, `"message"`, …; `min`/`max`/`curve` effective), `reuben_contract::{NUMBER_MIN, NUMBER_MAX}`.
- Produces:
  ```rust
  pub enum Scale { None, Linear { min: f64, max: f64, int: bool }, Exponential { min: f64, max: f64, int: bool } }
  pub struct Route { pub pattern: OscPattern, pub kind: MidiKind, pub target: String /* "/<bind>/in" */, pub scale: Scale }
  #[derive(Default)] pub struct RouteTable { pub routes: Vec<Route> }
  pub fn build_routes(doc: &SurfaceDoc, boundary: &Boundary) -> (RouteTable, Vec<String> /* warnings */);
  pub fn scale_value(scale: &Scale, kind: MidiKind, args: &[Arg]) -> Vec<Arg>;
  ```

- [ ] **Step 1: Write failing tests** for `build_routes`, each asserting the route is skipped and one warning names the control's `bind` and reason: no `midi`; malformed pattern; `MixedKinds`; `bind` not in `boundary.inputs`; `cc` → `"message"`; `note` → `"value"`; `program` → `"value"`; `cc` → `"signal"` without `min`/`max`; `cc` → `"value"` with min ≤ `NUMBER_MIN` and max ≥ `NUMBER_MAX` and no narrowing; and accepted cases: `cc` → `"signal"` with a range, `cc` → `"int"`, `program` → `"int"`, `note` → `"message"`, a doc `instrument` ≠ `boundary.instrument` (route kept, one warning). Narrowing outside the input range is clamped into it with a warning.
- [ ] **Step 2: Write failing tests** for `scale_value`: CC `[F32(64/127)]` into `Exponential{min:200,max:8000}` → `≈1284.0` (±0.5); `[F32(0.0)]` → `200`; `[F32(1.0)]` → `8000`; bend `[F32(0.0)]` into `Linear{min:-2,max:2}` → `0.0`; `Linear{int:true,min:0,max:16}` with `0.5` → `I32(8)`; **`Exponential{min:0,max:10}` with `0.5` → `5.0` (linear fallback, Review Focus 3)** and `{min:-10,max:10}` → `0.0`; `program` and `note` pass through unchanged.
- [ ] **Step 3: Run** `cargo test -p reuben-native midi::routes` — FAIL.
- [ ] **Step 4: Implement.** Effective range = control's `min`/`max` clamped into the input's. `Scale::Exponential` iff `curve == Some("exponential")`.
- [ ] **Step 5: Run — PASS. Commit** `feat(midi): build routes from a MIDI surface doc; scale into the input's range`.

### Task 5: The router, with held notes and counters

**Files:**
- Create: `crates/reuben-native/src/midi/router.rs`
- Modify: `crates/reuben-native/src/diagnostics.rs` (four `AtomicU64`s: `midi_received`, `midi_unsupported`, `midi_routed`, `midi_unmatched`, and their `Snapshot` fields)

**Interfaces:**
- Consumes: `ControlProducer` (`control::ring_pair`), `OscIn::new`, `RouteTable`, `scale_value`, `translate`.
- Produces:
  ```rust
  pub struct Router { /* producer, Arc<RouteTable>, held: BTreeSet<(String /*target*/, u8 /*ch*/, u8 /*pitch*/)>, diagnostics, log: bool */ }
  pub type SharedRouter = Arc<Mutex<Router>>;
  impl Router {
      pub fn new(producer: ControlProducer, diagnostics: Arc<Diagnostics>, log: bool) -> Self;
      pub fn bytes(&mut self, bytes: &[u8]);           // device callback entry
      pub fn translated(&mut self, t: &Translated);    // injection entry; bytes() calls it
      pub fn install(&mut self, table: RouteTable);    // releases each held note whose target is no note route's target in `table`
      pub fn release_all(&mut self);                   // device lost
  }
  ```
  One `translated` call pushes **one** batch holding every matched `/<bind>/in` message. Held notes are keyed by target, channel, pitch; a note-off removes the entry; release sends `[F32(pitch), F32(0.0)]` to the stored target.

- [ ] **Step 1: Write failing tests** draining the ring with `ControlDrain::new(vec![ring]).drain(..)`:
  `routes_a_cc_to_every_matching_input` (two routes on one pattern → one batch, two messages);
  `unmatched_is_counted_and_sends_nothing` with an empty table (Review Focus 5);
  `install_releases_notes_whose_route_is_gone`; `install_keeps_notes_whose_route_survives`;
  `retriggered_note_is_held_once` — two note-ons, one note-off, then `install(empty)` sends no extra note-off (Review Focus 4);
  `release_all_sends_note_off_for_each_held_note`;
  counters: `midi_received`, `midi_routed`, `midi_unmatched`, `midi_unsupported` move as expected; `Ignored` bytes move none.
- [ ] **Step 2: Run** `cargo test -p reuben-native midi::router` — FAIL.
- [ ] **Step 3: Implement.** With `log`, `eprintln!` each translated address and its targets.
- [ ] **Step 4: Run — PASS. Commit** `feat(midi): router — match, scale, deliver; release held notes`.

### Task 6: The `send_midi` engine-channel verb, CLI, and report fields

**Files:**
- Modify: `crates/reuben-api/src/engine/wire.rs` — `Request::SendMidi { address: String, args: Vec<ControlArg> }`; `Response::Sent` reused; `DiagnosticsReport` gains `#[serde(default)] pub midi_received/midi_unsupported/midi_routed/midi_unmatched: u64`.
- Modify: `crates/reuben-api/src/engine/server.rs` — `EngineHost::deliver_midi(&self, address: &str, args: Vec<Arg>) -> Result<(), String>` defaulting to `Err("this engine reads no MIDI")`; `dispatch_request` arm.
- Modify: `crates/reuben-api/src/engine/channel.rs` — `pub fn send_midi(&self, address: String, args: Vec<ControlArg>) -> Result<SendOutcome, ChannelError>`.
- Modify: `crates/reuben-native/src/engine_channel.rs` — `NativeHost` holds `Option<SharedRouter>` (`with_midi_router`); `deliver_midi` runs `parse_translated` (refusing with a message naming the bad address) then `Router::translated`; `diagnostics_report` maps the four counters.
- Modify: `crates/reuben-native/src/cli.rs`, `crates/reuben-native/src/bin/reuben.rs` — `reuben send-midi <ADDRESS> <VALUE>...` with `RunningServerFlags`.

- [ ] **Step 1: Write failing tests:** wire round-trip for `SendMidi`; `diagnostics_report_maps_every_counter_field_for_field` extended with the four fields; a `NativeHost` test that `deliver_midi("/midi/1/cc/74", [F32(0.5)])` reaches the router's ring and `deliver_midi("/midi/17/cc/1", …)` is `Err`; a host without a router answers the default refusal.
- [ ] **Step 2: Run** `cargo test -p reuben-api engine` and `cargo test -p reuben-native engine_channel` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run — PASS. Commit** `feat(engine): send_midi verb injects a translated MIDI message`.

### Task 7: Wire the router into `play`; rebuild routes on swaps and map edits

**Files:**
- Create: `crates/reuben-native/src/midi/watch.rs`
- Modify: `crates/reuben-native/src/engine.rs` (`start_with`: a third `ring_pair` for MIDI added to `ControlDrain::new`; build the `SharedRouter`; hand it to `NativeHost::with_midi_router`; spawn `RouteWatch` after the channel state exists)

**Interfaces:**
- Consumes: `Engine::channel()` / `Channel::{subscribe, get_document, call}` (call `names::DESCRIBE_BOUNDARY` with `DescribeBoundary { source }`, as `crates/reuben-editor/src/controls/mod.rs` `load` does), `locate_surface(_, ".midi")`, `SurfaceDoc::parse`, `build_routes`, `Router::install`.
- Produces: `pub(crate) struct RouteWatch` — `spawn(channel: Channel, router: SharedRouter, follow_files: bool) -> io::Result<Self>`; stops and joins on drop (pattern: `DroppedSendWatch`, `engine.rs`).

Behavior: rebuild at start, on every `Event::Opened`/`Event::Changed`, and (when `follow_files`) when the map file's mtime changes, polled every 250 ms. A rebuild prints each warning to stderr prefixed `midi map:`. A map that is missing installs an empty table; **a map that exists but fails to parse keeps the current table** and warns (Review Focus 2).

- [ ] **Step 1: Write failing integration tests** in `crates/reuben-native/tests/midi_routes.rs`, using a headless start (`start_headless`, as other native tests do) on a temp dir holding `inst.json` (one `f32` input `cutoff`, `min` 20, `max` 20000, `exponential`; one `note` input `notes`) and `surfaces/inst.midi.json`:
  `injected_cc_is_routed` — `channel.send_midi("/midi/2/cc/74", [0.5])` → `get_diagnostics().midi_routed == 1`;
  `unmatched_injection_counts_unmatched`;
  `editing_the_map_reroutes` — rewrite the map to bind channel 3, wait for the rebuild (poll diagnostics ≤ 2 s), channel 3 routes and channel 2 does not;
  `invalid_map_keeps_previous_routes` (Review Focus 2);
  `no_map_routes_nothing`.
- [ ] **Step 2: Run** `cargo test -p reuben-native --test midi_routes` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run — PASS. Full gates. Commit** `feat(play): route MIDI through the open instrument's MIDI surface doc`.

### Task 8: Open MIDI devices from the device profile

**Files:**
- Modify: `Cargo.toml` (`midir = "<current>"` under workspace deps), `crates/reuben-native/Cargo.toml` (`midir.workspace = true`)
- Modify: `crates/reuben-native/src/profile.rs` — `DeviceProfile` gains `#[serde(default)] pub midi: MidiProfile` with `pub struct MidiProfile { #[serde(default)] pub inputs: Vec<String> }` (`deny_unknown_fields` stays)
- Modify: `crates/reuben-native/schema/device-profile.schema.json`
- Create: `crates/reuben-native/src/midi/devices.rs`
- Modify: `crates/reuben-native/src/engine.rs` (open devices in `start_with`; keep connections alive in `RunningEngine`), `StartError` (new `MidiDevice` variant)

**Interfaces:**
- Produces:
  ```rust
  pub fn select_port(available: &[String], wanted: &str) -> Option<usize>; // exact case-insensitive, else first containing
  pub fn no_match_message(wanted: &str, available: &[String]) -> String;  // names `wanted` and lists every available port
  pub(crate) struct MidiInputs { /* midir connections */ }
  impl MidiInputs { pub(crate) fn open(names: &[String], router: SharedRouter) -> Result<Self, String>; }
  ```
  Each connection's callback calls `router.lock().bytes(msg)`. A port-poll thread (1 s) calls `router.lock().release_all()` and warns `midi device <name> is gone` when an opened port's name leaves `MidiInput::ports()`. Opening failures on Windows say the port may be held by another program.

- [ ] **Step 1: Write failing tests:** profile parses `{"midi":{"inputs":["Keystep"]}}`; an unknown key under `midi` is a `ProfileError`; `select_port(["Arturia KeyStep 32","MPC"], "keystep")` → `Some(0)`, `"mpc"` → `Some(1)` (exact wins over substring when both exist), `"nano"` → `None`; `no_match_message` contains every available name.
- [ ] **Step 2: Run** `cargo test -p reuben-native profile midi::devices` — FAIL.
- [ ] **Step 3: Implement.** No `midi` section opens nothing and spawns no poll thread.
- [ ] **Step 4: Run — PASS. Full gates.** Manual check, if a device is attached: `reuben play <inst> --io-map <profile> --log-osc`, play keys, see translated addresses logged.
- [ ] **Step 5: Commit** `feat(play): open MIDI inputs named in the device profile`.

### Task 9: Docs

**Files:**
- Modify: `docs/device-profile.md` (the `midi.inputs` section)
- Modify: `.claude/skills/control-surface/SKILL.md` (the MIDI surface doc: file name, `midi` field, the address table, the per-control checks, scaling, `reuben send-midi` for testing)
- Modify: `docs/agents/authoring.md` (one paragraph under the surface-doc mention: MIDI maps exist, live in `surfaces/<stem>.midi.json`, and never change the Instrument)

- [ ] **Step 1: Write the docs** with values copied from the spec.
- [ ] **Step 2: Run** `cargo test -p reuben-guide` (authoring.md slicing) and `cd eval && python3 -m reuben_eval.gate` (authoring.md feeds the agent surface) — PASS.
- [ ] **Step 3: Commit** `docs: MIDI surface docs and the device profile's midi inputs`.

## PR split

- **PR 1:** Tasks 1–7 — everything testable without hardware, exercised end to end by `send_midi`.
- **PR 2:** Tasks 8–9 — devices and docs.

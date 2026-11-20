# Node Meters Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every node on the editor canvas level being viewed shows a live level meter while the engine plays.

**Architecture:** `reuben-core` measures each probed node's Signal outputs right after the node runs, into per-Plan atomic slots, using the master meter's ballistics (moved down from `reuben-native`). The native structure host keeps a leased probe set, flips slot flags after every Swap, and reports levels through `GetMeters`. The editor sends the visible level's nodes as `SetProbes` and paints a bar per Signal output on each box.

**Tech Stack:** Rust workspace; `reuben-core` (`no_std` + `alloc`), `reuben-api` (engine wire + server), `reuben-native` (structure host), `reuben-editor` (eframe/egui, `egui_kittest`).

**Spec:** `docs/superpowers/specs/2026-11-20-node-meters-design.md`

## Global Constraints

- RT-safe Render: nothing added to `render_plan` or `Operator::process` paths may allocate, lock or block. `Relaxed` atomics only.
- Determinism: render output is bit-identical with probes on and off.
- `reuben-core` stays `no_std` + `alloc`; no 64-bit atomics in core (the `reuben-api` `render` feature cross-builds for bare metal).
- Ballistics: peak falls at `PEAK_FALL_DB_PER_SEC` (12.0), RMS window `RMS_WINDOW_SECS` (0.3), floor `METER_FLOOR_DBFS` (−120), ceiling `METER_CEILING_DBFS` (60) — one implementation shared by master and node meters.
- Lease: the probe set expires 5 s after the last `SetProbes`; only `SetProbes` renews it. The editor re-sends every 2 s.
- Cap: at most 512 paths per `SetProbes`; more is refused.
- The MCP tool `get_engine_meters`'s advertised output schema does not change.
- Only Signal output ports are metered; a node with none has no slot and no meter.
- Repo rules (CLAUDE.md): no issue numbers in comments; comments state mechanics, not restatement; gates are `cargo fmt --all --check`, `cargo clippy --all-targets --features reuben-document/bench -- -D warnings`, `cargo clippy -p reuben-editor --all-targets -- -D warnings`, both `cargo doc` gates, `cargo test`, `cargo test -p reuben-editor`. Never add `--workspace`.
- PRs go against `dev`. One PR for Tasks 1–3 (core), one for 4–5 (wire + host), one for 6 (editor) is the suggested split; each must pass CI on its own.

## Review Focus

1. **A probe enabled after being off reads its old level** — a node re-entered after navigation must start from silence, not from a frozen peak. Test in Task 5 (`re_enabling_a_probe_starts_from_silence`).
2. **A Voicer with no voice sounding** must still decay its slots each block, so a released note's meter falls rather than freezing. Test in Task 3 (`a_silent_voicer_still_decays_its_meters`).
3. **A Swap that renames or removes a probed node** must drop it from the report without error, and a re-added address must come back. Test in Task 5 (`a_swap_drops_a_renamed_probe_and_keeps_the_rest`).
4. **A NaN or blown-up node output** must clamp to the ceiling and recover, as the master does, not poison the slot forever. Test in Task 2 (`a_blown_up_node_meters_at_the_ceiling_and_recovers`).
5. **The editor closing** must stop the engine metering within the lease. Test in Task 5 (`an_unrenewed_probe_set_expires`).

---

### Task 1: Move the meter fold and ballistics into `reuben-core`

**Files:**
- Create: `crates/reuben-core/src/meter.rs`
- Modify: `crates/reuben-core/src/lib.rs` (add `pub mod meter;`)
- Modify: `crates/reuben-native/src/meters.rs` (`Bank` becomes a wrapper over `meter::Level`s plus its `AtomicU64` frame count)
- Modify: `crates/reuben-api/src/engine/wire.rs:471-477` (`PEAK_FALL_DB_PER_SEC`, `RMS_WINDOW_SECS` become `pub use reuben_core::meter::{…}`; their doc text stays where it is advertised)

**Interfaces:**
- Produces, in `reuben_core::meter`:
  - `pub const PEAK_FALL_DB_PER_SEC: f32 = 12.0; pub const RMS_WINDOW_SECS: f32 = 0.3;`
  - `pub struct Ballistics { /* per-frame ln factors */ }` with `pub fn new(sample_rate: f32) -> Self` and `fn keep(&self, frames: usize) -> (f32 /*peak_fall*/, f32 /*rms_keep*/)`.
  - `pub struct Block { pub peak: f32, pub sum_squares: f32 }` with `pub fn measure(samples: impl Iterator<Item = f32>) -> Block` and `pub fn merge(&mut self, other: Block)` (peak = max, sum_squares = sum).
  - `#[derive(Debug, Default)] pub struct Level { peak: AtomicU32, mean_square: AtomicU32 }` with `pub fn commit(&self, block: Block, frames: usize, ballistics: &Ballistics)` (applies the blown-up clamp, decay, flush — the body of today's per-channel loop in `Bank::meter`), `pub fn read(&self) -> (f32, f32)` (linear peak, mean square), `pub fn reset(&self)`.
  - The constants `BLOWN_UP_PEAK`, `BLOWN_UP_MEAN_SQUARE`, `FLUSH_BELOW` move here with their comments.

- [ ] **Step 1:** Move the existing `#[cfg(test)]` tests of `crates/reuben-native/src/meters.rs` that test the fold (not the bank's interleaving or frame count) to `meter.rs`, rewritten against `Level::commit`/`Block::measure`. Add `merged_blocks_take_the_loudest_peak_and_sum_their_squares`: `Block{peak:0.5,sum_squares:2.0}.merge(Block{peak:0.25,sum_squares:1.0})` gives `peak == 0.5`, `sum_squares == 3.0`.
- [ ] **Step 2:** Run `cargo test -p reuben-core meter::` — FAIL (module missing).
- [ ] **Step 3:** Implement `meter.rs` as above; rebuild `native::meters::Bank::meter` on `Block::measure` + `Level::commit`, keeping its signature and its frame counter.
- [ ] **Step 4:** Run `cargo test -p reuben-core meter::` and `cargo test -p reuben-native meters` — PASS, native's remaining bank tests unchanged. Run `cargo test -p reuben-api --all-features the_advertised_levels_name_the_constants` — PASS.
- [ ] **Step 5:** Commit: `Core: the level meter's fold and ballistics, shared with the device banks`.

### Task 2: Probe slots on the Plan, folded as each node runs

**Files:**
- Create: `crates/reuben-core/src/probe.rs`
- Modify: `crates/reuben-core/src/plan.rs` (`Plan.probes`, `PlanNode.probe`, built in `Plan::instantiate`)
- Modify: `crates/reuben-core/src/render.rs:385-400` (after `process_node` in `render_plan`'s loop; commit at block end)
- Test: `crates/reuben-core/src/probe.rs` tests; `crates/reuben-document/tests/rt_alloc/mod.rs`

**Interfaces:**
- Consumes: Task 1's `Ballistics`, `Block`, `Level`.
- Produces, in `reuben_core::probe`:
  - `pub struct ProbeSlots { pub nodes: Vec<NodeProbe>, ballistics: Ballistics }`, with `pub fn find(&self, address: &str) -> Option<&NodeProbe>`.
  - `pub struct NodeProbe { pub address: Arc<str>, pub ports: Vec<PortProbe>, pub hosted: Option<Arc<ProbeSlots>>, on: AtomicBool, /* block accumulators */ }` with `pub fn set_on(&self, on: bool)` (turning on calls `reset` on every port), `pub fn is_on(&self) -> bool`.
  - `pub struct PortProbe { pub name: Arc<str>, pub level: Level, /* accumulator: peak + sum_squares as AtomicU32 f32 bits */ }`.
  - `impl ProbeSlots { pub(crate) fn accumulate(&self, node: usize, port: usize, block: Block); pub fn commit(&self, frames: usize) }` — `commit` folds every on node's accumulators into its `Level`s once and clears them; off nodes are skipped.
  - `Plan { pub probes: Arc<ProbeSlots>, … }`; `PlanNode { pub probe: Option<usize>, … }` (index into `probes.nodes`; `None` for a node with no Signal output).
  - `Plan::hosted: bool` (private), `false` from `Plan::instantiate`.
- Render: in `render_plan`, after `process_node(i)`, if `plan.nodes[i].probe` is `Some(p)` and `probes.nodes[p].is_on()`, measure each `outputs[k]` arena buffer over `frames` and `accumulate`. At the end of `render_plan`, if `!plan.hosted`, `probes.commit(frames)`.

- [ ] **Step 1: Write the failing tests** in `probe.rs` (build a `/osc` → `/out` graph as `render.rs`'s `install_graph` test does):
  - `a_probed_oscillator_reads_its_level`: probe `/osc` on, render 1 s of blocks; `read()` peak ≈ 1.0 (within 1e-3) and mean square ≈ 0.5 (within 1e-2).
  - `an_unprobed_node_is_not_measured`: probe off; levels stay `(0.0, 0.0)`.
  - `probes_do_not_change_what_renders`: two identical plans, one with every probe on; master output bit-identical over 100 blocks.
  - `a_blown_up_node_meters_at_the_ceiling_and_recovers` (Review Focus 4): a node writing `f32::NAN` for one block reads peak `1000.0`; after 10 s of zeros its peak is below 1e-4.
  - `a_message_only_node_has_no_slot`: a node with no Signal output has `probe == None`.
- [ ] **Step 2:** Run `cargo test -p reuben-core probe::` — FAIL.
- [ ] **Step 3:** Implement `probe.rs`, the Plan fields, and the render hook as specified.
- [ ] **Step 4:** Add to `crates/reuben-document/tests/rt_alloc/mod.rs` a case `render_with_every_probe_on_allocates_nothing` mirroring its existing render case with every `probes.nodes[*].set_on(true)`. Run `cargo test -p reuben-core probe::` and `cargo test -p reuben-document --test rt_alloc` — PASS.
- [ ] **Step 5:** Run the render bench gate locally if it exists for the change (`crates/reuben-document/benches/README.md`), confirm probes-off cost is within noise. Commit: `Core: per-node probe slots, measured as each node runs`.

### Task 3: Voicer voices share one slot set, committed once per block

**Files:**
- Modify: `crates/reuben-core/src/operator.rs:620-642` (new default method)
- Modify: `crates/reuben-core/src/plan.rs` (`Plan::instantiate_hosted`; fill `NodeProbe.hosted` after each node's `on_instantiate`)
- Modify: `crates/reuben-core/src/operators/voicer.rs:248-280` and its `process`

**Interfaces:**
- Consumes: Task 2's `ProbeSlots`, `Plan::probes`, `Plan::hosted`.
- Produces:
  - `Operator::hosted_probes(&self) -> Option<Arc<ProbeSlots>> { None }` (default).
  - `Plan::instantiate_hosted(graph: Graph, config: AudioConfig, probes: Arc<ProbeSlots>) -> Result<Plan, PlanError>` — uses the given slots (built from the first voice graph) and sets `hosted = true`.
  - `ProbeSlots::for_graph(graph: &Graph, config: &AudioConfig) -> Arc<ProbeSlots>` (shared by both instantiate paths).
  - The Voicer builds one `ProbeSlots` from `graphs[0]`, instantiates every voice with `instantiate_hosted`, returns it from `hosted_probes`, and calls `probes.commit(frames)` once at the end of every `process`, sounding voices or not.

- [ ] **Step 1: Write the failing tests** in `voicer.rs`'s tests:
  - `voices_fold_into_one_meter_that_decays_once_per_block`: two voices holding the same note, probe the voice `/osc` on via `hosted_probes()`; peak ≈ 1.0 (each voice's own peak, not 2.0); after the notes end, the peak after 1 s has fallen by 12 dB (±0.5 dB), not 24.
  - `a_silent_voicer_still_decays_its_meters` (Review Focus 2): after release and the voices go idle, the peak keeps falling at 12 dB/s.
  - `two_voicers_on_one_voice_patch_meter_separately`: two Voicers bound to the same voice graph; only one plays; the other's `/osc` reads `(0.0, 0.0)`.
  - In `plan.rs` tests: `a_hosting_node_exposes_its_voices_slots`: the outer Plan's Voicer `NodeProbe.hosted` is `Some` and holds `/osc`.
- [ ] **Step 2:** Run `cargo test -p reuben-core voicer:: plan::` — FAIL.
- [ ] **Step 3:** Implement as specified. A Voicer node itself has Signal outputs and keeps its own outer slot as any node does.
- [ ] **Step 4:** Run `cargo test -p reuben-core` and the rt_alloc test — PASS.
- [ ] **Step 5:** Commit: `Core: a Voicer's voices share one probe slot set`. Open the core PR (Tasks 1–3) against `dev`.

### Task 4: `SetProbes` and `probes` on the engine wire

**Files:**
- Modify: `crates/reuben-api/src/engine/wire.rs` (request, report types)
- Modify: `crates/reuben-api/src/engine/server.rs` (`EngineHost` methods, dispatch, call after install)
- Modify: `crates/reuben-api/src/engine/channel.rs` (client method)
- Modify: `crates/reuben-document/src/coordinator/swap.rs:455-466` and `:133`, `:339` (keep the installed Plan's `Arc<ProbeSlots>`)
- Modify: `crates/reuben-mcp/src/lib.rs:416-426` (tool output stays device-only)

**Interfaces:**
- Consumes: `reuben_core::probe::ProbeSlots`.
- Produces:
  - `Request::SetProbes { paths: Vec<Vec<String>> }`; `Response::Probes { resolved: Vec<Vec<String>> }`.
  - `pub const MAX_PROBES: usize = 512;` — over it the server answers `Response::Error` with `"at most 512 probes"` before calling the host.
  - `MetersReport { …, #[serde(default, skip_serializing_if = "Vec::is_empty")] pub probes: Vec<ProbeLevels> }`; `pub struct ProbeLevels { pub path: Vec<String>, pub ports: Vec<PortLevel> }`; `pub struct PortLevel { pub port: String, pub level: ChannelLevel }`.
  - `EngineHost::set_probes(&self, paths: Vec<Vec<String>>) -> Result<Vec<Vec<String>>, String>` defaulting to `Err("this engine does not meter nodes")`; `EngineHost::install_probes(&self, probes: Arc<ProbeSlots>)` defaulting to nothing.
  - `Coordinator::installed_probes(&self) -> Arc<ProbeSlots>`; the server calls `host.install_probes(coordinator.installed_probes())` after every successful swap (beside `publish_render_config`) and after the initial install.
  - `Channel::set_probes(&self, paths: Vec<Vec<String>>) -> Result<Vec<Vec<String>>, ChannelError>`.
  - MCP: `get_engine_meters` returns the report with `probes` emptied, so the field is never serialized; its `output_schema` is generated from a device-only type or with `probes` skipped — whichever keeps the advertised schema byte-identical.

- [ ] **Step 1: Write the failing tests:**
  - wire.rs: `set_probes_round_trips` (request and response through `to_ndjson`/parse); `a_report_without_probes_serializes_as_before` (a `MetersReport` with empty `probes` serializes to exactly today's JSON).
  - server.rs: `set_probes_past_the_cap_is_refused_without_reaching_the_host` (513 paths); `a_host_without_node_meters_says_so`.
  - reuben-mcp: `get_engine_meters_advertises_the_same_schema` — the tool's output schema JSON equals the schema of today's `MetersReport` fields (`outputs`, `inputs`, `frames`) exactly.
- [ ] **Step 2:** Run `cargo test -p reuben-api --all-features engine::` and `cargo test -p reuben-mcp get_engine_meters` — FAIL.
- [ ] **Step 3:** Implement as specified.
- [ ] **Step 4:** Run the tests — PASS. Run `cd eval && python3 -m reuben_eval.gate` — PASS with no token change.
- [ ] **Step 5:** Commit: `Engine wire: SetProbes, and node levels in GetMeters`.

### Task 5: The native host's leased probe set

**Files:**
- Create: `crates/reuben-native/src/probes.rs`
- Modify: `crates/reuben-native/src/structure.rs` (`NativeHost` implements `set_probes`, `install_probes`, and fills `meters().probes`)
- Modify: `crates/reuben-native/src/lib.rs` (`mod probes;`)

**Interfaces:**
- Consumes: Task 4's host methods and types; Task 2/3's `ProbeSlots`, `NodeProbe::{set_on, hosted}`, `Level::read`; `ChannelLevel::from_linear`.
- Produces: `pub(crate) struct Probes` behind a `Mutex` in `NativeHost`, with:
  - `fn set(&mut self, paths: Vec<Vec<String>>, now: Instant) -> Vec<Vec<String>>` — replaces the set, renews the lease, turns on exactly the resolved nodes (off for the rest), returns the resolved paths in request order.
  - `fn install(&mut self, slots: Arc<ProbeSlots>)` — re-resolves the held paths against the new slots.
  - `fn report(&mut self, now: Instant) -> Vec<ProbeLevels>` — first expires the set if `now` is 5 s past the last `set`, turning every flag off.
  - `const LEASE: Duration = Duration::from_secs(5);`
  - Path resolution: `path[0]` is found in the outer `ProbeSlots`; each further element in the previous node's `hosted` slots.

- [ ] **Step 1: Write the failing tests** in `probes.rs`, against `ProbeSlots` built from small graphs (outer `/osc`, `/out`, and a Voicer `/v` hosting `/osc`):
  - `a_path_resolves_through_a_hosting_node`: `[["/v","/osc"]]` resolves and turns on only the voice `/osc`.
  - `an_unknown_path_is_left_out_of_the_answer`.
  - `re_enabling_a_probe_starts_from_silence` (Review Focus 1).
  - `a_swap_drops_a_renamed_probe_and_keeps_the_rest` (Review Focus 3): `install` new slots without `/osc` but with `/out`; report lists `/out` only; installing slots with `/osc` again brings it back.
  - `an_unrenewed_probe_set_expires` (Review Focus 5): `report(now + 5 s)` is empty and every flag is off; `report(now + 4 s)` is not.
- [ ] **Step 2:** Run `cargo test -p reuben-native probes::` — FAIL.
- [ ] **Step 3:** Implement and wire into `NativeHost`.
- [ ] **Step 4:** Add an end-to-end test in `structure.rs`'s tests: headless engine, `Channel::set_probes([["/osc"]])` through the real channel, `get_meters()` within 1 s reports `/osc` above the floor. Run `cargo test -p reuben-native` — PASS.
- [ ] **Step 5:** Commit: `reuben play: meter the nodes a client probes, on a 5 s lease`. Open the wire + host PR (Tasks 4–5) against `dev`.

### Task 6: Editor — probe the visible level and paint node meters

**Files:**
- Modify: `crates/reuben-editor/src/meter_strip.rs` (the asking thread also sends `SetProbes`; `MeterStrip` holds the latest `probes`)
- Create: `crates/reuben-editor/src/canvas/meters.rs` (painting a box's bars)
- Modify: `crates/reuben-editor/src/canvas/view.rs:127` (`node` paints the box's meter), `crates/reuben-editor/src/canvas/mod.rs` (the visible boxes' probe paths; levels passed in to `show`)
- Modify: `crates/reuben-editor/src/session/mod.rs` (path prefix from the trail)
- Modify: `crates/reuben-editor/src/main.rs` (hand the strip the visible paths; hand the canvas the levels)

**Interfaces:**
- Consumes: Task 4's `Channel::set_probes`, `MetersReport::probes`, `ProbeLevels`, `PortLevel`.
- Produces:
  - `Session::probe_path(&self, address: &str) -> Vec<String>` — walks the trail: a crumb whose node is a `subpatch` adds its address as a prefix to the current segment; a crumb whose node hosts voices (`voicer`) closes the segment and starts a new one. Top level: `["/osc"]`; inside subpatch `/bass`: `["/bass/filter"]`; inside Voicer `/kick_v`: `["/kick_v", "/osc"]`.
  - `Canvas::probe_paths(&self, session: &Session) -> Vec<Vec<String>>` — one path per box drawn as a node; for a subpatch box, the paths of the inner nodes feeding each Signal interface output (resolved from the subpatch document the session reads).
  - `MeterStrip::poll(&mut self, now: Instant, channel: Option<Channel>, probes: &[Vec<String>])` — sends `SetProbes` on the asking thread when `probes` differs from what was last sent, or 2 s after the last send; never on the UI thread.
  - `MeterStrip::node_levels(&self) -> &[ProbeLevels]`.
  - `canvas::meters::paint(painter, rect, ports: &[PortLevel], visuals)` — one vertical bar per port along the box's right edge, RMS fill with a peak tick, `theme::METER`, the strip's −60..0 dBFS scale (`meter_strip::SCALE_FLOOR_DBFS`). Placement within the box is the implementer's call.

- [ ] **Step 1: Write the failing tests:**
  - session: `a_probe_path_carries_the_subpatch_prefix_and_splits_at_a_voicer` with the three examples above (check the spliced prefix against a real loaded subpatch's Plan addresses, not by assumption).
  - meter_strip: `a_changed_probe_set_is_sent_and_an_unchanged_one_renewed_every_two_seconds` with the `Canned` transport counting `SetProbes` lines.
  - ui_tests: `a_playing_node_shows_its_level_on_the_canvas`: embedded headless engine on an instrument with `/osc` → `/out`; after polling, the canvas exposes an AccessKit `ProgressIndicator` labelled `"/osc level"` with value above the floor; a node with no Signal output exposes none.
- [ ] **Step 2:** Run `cargo test -p reuben-editor probe_path meter_strip:: a_playing_node` — FAIL.
- [ ] **Step 3:** Implement as specified. The canvas paints with no levels (nothing drawn) when the strip has none.
- [ ] **Step 4:** Run `cargo test -p reuben-editor`, `cargo clippy -p reuben-editor --all-targets -- -D warnings`, `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p reuben-editor` — PASS. Launch the editor on `instruments/acid-techno.json`, enter a subpatch and a Voicer, and confirm meters move and follow navigation.
- [ ] **Step 5:** Commit: `Editor: a level meter on every node of the visible level`. Open the editor PR against `dev`.

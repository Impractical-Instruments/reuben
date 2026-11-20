# Node meters on the editor canvas

Design for GitHub issue #883 ("Per-node meters (probes) inside an instrument").

## Intent

While someone plays an instrument in `reuben-editor`, every node on the canvas level they are
looking at shows a level meter. They can see at a glance where sound flows through the patch and
where it stops — a filter closed to silence, one side of a `mix` bus dead, a subpatch producing
nothing.

The master meter strip (engine meters, `GetMeters`) shows only the device. This extends the same
ballistics to points inside the instrument.

## Goals

- A meter on each node of the visible canvas level, live while the engine plays, in both the
  embedded and the attached (`--attach`) editor.
- The engine measures only nodes someone is looking at: the cost follows the visible set, not the
  instrument's size.
- Moving between canvas levels (entering a subpatch, leaving it) changes what is measured without
  rebuilding the Plan and without a Swap. Nothing the player hears changes because the meter set
  changed.
- The same ballistics as the master strip: peak falling at `PEAK_FALL_DB_PER_SEC`, RMS averaged over
  `RMS_WINDOW_SECS`, the same floor and ceiling, one implementation.

## Non-goals

- Probes stored in the instrument document. The set is the editor's view, not part of the sound;
  nothing is written to the document.
- Meters for an AI agent (MCP tool). The wire is usable by any structure-channel client, but no MCP
  tool is added here.
- Message (OSC-shaped) outputs. Only Signal outputs get a level; a message-only node shows no meter.
- Scopes, spectra, per-voice breakdowns, clip latching on node meters.

## Anti-goals

- Any audible effect: metering must not change render output (determinism) and must not allocate,
  lock or block on the audio thread (RT-safe Render).
- A probe change that costs a Swap or a Plan rebuild.
- Probes left running forever after the editor that asked for them is gone.

## Constraints from the code

- **Arena buffers are recycled by liveness** (`crates/reuben-core/src/plan.rs`, "Arena liveness"):
  a node's output buffer is handed to a later node once its last consumer has run. Only master taps
  and Signal interface outputs are pinned. So a node's output can only be read *right after that
  node runs*, unless it is pinned at Instantiate.
- **Subpatches are spliced** into the parent Plan: an inner node is an ordinary Plan node whose
  address carries the parent's prefix (`/bass/filter`).
- **Voicer voices are separate Plans** rendered with the re-entrant `render_plan` once per voice per
  block (`crates/reuben-core/src/operators/voicer.rs`). A node inside a voice runs N times a block.
- **`reuben-core` is `no_std` + `alloc`**, and the `reuben-api` `render` feature is cross-built for
  bare metal. 64-bit atomics are not available on every such target (`thumbv7em` has none).
- **The structure channel is one connection per exchange**; the engine cannot tell clients apart.

## Approaches

Considered:

- **A. Measure in `reuben-core` as each node runs (chosen).** Each Plan carries a metering slot per
  node. After `process_node`, a flagged node's Signal outputs are folded into its slot while the
  buffer still holds them. Flags are set off the audio thread. No pinning, no rebuild.
- **B. An observer hook in core, metering in native.** Core calls a host-supplied observer with each
  flagged node's output slices; `reuben-native` folds them into its `Bank`s. Keeps the ballistics in
  one place without moving them, but puts a trait object on the render path, and the Voicer's nested
  `render_plan` must thread the hook into every voice.
- **C. Pin probe buffers at Instantiate, meter at the end of the block.** Simplest render-side, but
  every change to the probe set is a rebuild and a Swap. Ruled out: with a meter on every visible
  node, the set changes on every canvas navigation.

**Picked: A, with the master meter's `Bank` (ballistics and fold) moved from
`crates/reuben-native/src/meters.rs` into `reuben-core`**, so master and node meters share one
implementation. Chosen by the user over B and C.

## Design

### Core: the fold and the slots

- Move `Bank`'s per-channel level state, its constants (`BLOWN_UP_*`, `FLUSH_BELOW`) and its fold
  into `reuben-core` (a `meter` module). `reuben-native` keeps using it for the device banks; the
  `frames` counter stays where it is useful. *Builder's call:* how the frame counter survives on
  targets without `AtomicU64` (`cfg(target_has_atomic = "64")`, or an `AtomicU32` wrapping count).
- A Plan gets one **probe slot per plan node with at least one Signal output**, one level (peak,
  mean square — `f32` bits in `AtomicU32`s) per Signal output port, plus an `AtomicBool` "on" flag
  per node. Allocated at Instantiate (off the audio thread), shared (`Arc`) so a reader outside
  render can read levels and write flags.
- In `render_plan`, after a flagged node's `process_node`, fold each of its Signal output buffers into
  its slot. `Relaxed` throughout: a flag read late by one block only delays a meter by one block.
- **Voices share slots, per Voicer.** Each Voicer builds one slot set for its voice patch at
  `on_instantiate` and hands the same `Arc` to every voice Plan it instantiates (a
  `Plan::instantiate` variant that takes the slots rather than allocating its own). Two Voicers
  playing the same voice file have separate slot sets: they are separate canvas levels
  (`/kick_v`, `/snare_v`). The host reaches a Voicer's slots through a new `Operator` method with a
  default of "hosts nothing" (*my call*), so only hosting operators implement it. Within a block every voice adds into the same accumulators; the decay is
  applied **once per block**, not once per voice. The block's peak is the loudest voice's peak; its
  mean square is the sum of the voices' mean squares. *Builder's call:* the exact mechanics of
  accumulate-then-commit (e.g. a per-slot block accumulator committed by the Voicer after its voice
  loop, or a block counter in the slot).

### Addresses and Swap

- A probe is named by a **path**: a list of addresses from the outer Plan down through hosting
  nodes (*my call*). `["/bass/filter"]` is a node of the outer Plan (a spliced subpatch child keeps
  its parent's prefix); `["/kick_v", "/osc"]` is the `/osc` node of the voice patch the Voicer at
  `/kick_v` hosts. A voice patch's addresses carry no Voicer prefix and can repeat an outer
  address, so a flat string would be ambiguous. Levels are reported per Signal output port by name.
- **A subpatch's own box** on the parent level meters what feeds each of its interface outputs
  (*my call*): the subpatch node does not exist in the Plan (it is spliced), and its output pipes
  are usually dissolved at Instantiate. The editor resolves each interface output's `from` to the
  inner node's path (it reads the subpatch document already) and probes that. A box shows one bar
  per Signal interface output. An output whose `from` is itself a pipe the Plan dissolved resolves
  through the chain; *builder's call* how far that needs to go.
- **Voice levels are in scope:** the user asked for every node on the visible level, and a voice
  patch opened from its Voicer is a level.
- The engine host keeps the requested address set. After every Swap installs, it maps the set onto
  the new Plan's slots and sets their flags; an address the new Plan does not have is skipped
  silently and reports nothing. Nothing about this touches the render path except the flag stores.

### Where the set lives

- In **`reuben-native`'s structure host** (`crates/reuben-native/src/structure.rs`), not the
  portable `reuben-api` server (*my call*): the bare-metal render build carries no lease clock and
  no cap. The `reuben-api` host trait gains a `set_probes` method whose default answers
  "unsupported", the way `meters()` defaults to empty.
- The host keeps the requested paths and, after each Swap installs, maps them onto the new Plan's
  slots (including through hosting operators). So the Plan's slot `Arc` has to be reachable by the
  host after install; *builder's call* how it travels through `install_graph` / `RenderSlot`.

### Wire (`reuben-api` engine channel)

- New request **`SetProbes { paths: Vec<Vec<String>> }`**: replaces the engine's whole probe set.
  Answer: the paths it resolved (so a client can tell a stale address from a silent node).
- **`GetMeters`' `MetersReport` gains `probes`**: one entry per resolved, metered node — its path,
  and per Signal output port the port name and a `ChannelLevel`. Empty when no probes are set.
  Additive on the wire.
- **The MCP tool `get_engine_meters` does not show `probes`** (*my call*): its output stays the
  device levels it advertises today, so its schema and the eval gate's token count do not move. It
  returns a device-only shape, or strips the field; *builder's call* which.
- **One probe set per engine, last writer wins.** The channel cannot tell clients apart; two editors
  attached to one engine would fight over it. Accepted: the normal case is one editor.
- **Lease:** the set expires if no `SetProbes` arrives for 5 s (*my call*). Only `SetProbes`
  renews it — not `GetMeters`, which an agent may poll while the editor that set the probes is
  gone. The editor re-sends its set every 2 s while it has one. A closed or crashed editor leaves
  the engine metering nothing within 5 s.
- A cap on the set's size (*my call: 512 paths*), refused beyond it, so a malformed request
  cannot ask for unbounded work. Real visible levels are tens of nodes.

### Editor

- The canvas knows which nodes the visible level shows. When that set changes (open, navigation,
  edit, Swap), and every 2 s as the lease renewal, the editor sends `SetProbes` with their paths
  (the session's trail gives the subpatch prefix or the hosting Voicer); the existing meter-strip poll
  reads `probes` from the same `GetMeters` reply — no second poll.
- Each node box on the canvas draws a small meter: one thin bar per Signal output port (a stereo
  `pan` shows two), RMS bar with peak tick, the same colours and −60..0 dBFS scale as the strip.
  Nodes with no Signal output draw no meter. *Builder's call:* exact placement and size on the box.
- Attached mode goes through the link's channel like the strip's poll, so it inherits the same
  off-UI-thread behaviour.

## Left to the builder

- Module and type names in core; how `Bank` is split between the reusable fold and the device
  wrapper.
- The 64-bit-atomics question for the frame counter.
- The Voicer accumulate-then-commit mechanics.
- Meter placement on the node box.
- Tests: an RT-safety check that metering allocates nothing (the repo's existing alloc-counting
  tests), a determinism check that render output is bit-identical with probes on and off, a Voicer
  test that N voices decay once per block, a test that two Voicers on one voice file meter
  separately, a Swap test that a renamed node drops out, a lease expiry test, a test that the MCP
  tool's advertised schema is unchanged, and an editor UI test that a visible node's meter moves while playing.

# Sample player and the resource store: decoded audio as a shared, bank-ready read service

## Context

The [v1.1 roadmap](../../ROADMAP.md) flags the **sample player** as the one remaining
operator, blocked on the "Format & library" thread: it needs *sample-data references in the
JSON format* plus a *non-RT resource-load step*. Every prior operator is a pure function of
params + edges; the sample player is the first to depend on **external bytes** (an audio file)
that must be resolved, decoded, and held in memory before render.

Three existing contracts make that awkward, and grilling the design was about reconciling
them rather than bolting on a special case:

- **Construction is zero-arg + type-erased.** The registry builds operators through
  `make: fn() -> Box<dyn Operator>` ([`registry.rs`](../../crates/reuben-core/src/registry.rs))
  — no slot to hand in a decoded buffer or a file path.
- **Params are `f32`-only.** A node's overrides are `BTreeMap<String, f32>`
  ([`format.rs`](../../crates/reuben-core/src/format.rs)); a *sample reference* is a string.
- **Render is RT-safe and `process` must not allocate** ([ADR-0001](0001-unified-block-graph-execution.md),
  [ADR-0012](0012-boundary-and-threading.md)). Decoding a WAV is none of those things, so it
  cannot happen on the audio thread — it belongs to an authoring-time step, like
  [`format::load`](../../crates/reuben-core/src/format.rs).

A **forward constraint** shaped the whole tree: the long-term goal is an *audio bank* whose
total decoded size can exceed RAM, served by a block cache warmed on demand. v1.1 will not
build streaming, but it must not pick a seam that streaming would force us to rip out.

This ADR settles: how a node references audio, who owns the decoded data and how it is shared,
how the data reaches a type-erased operator, and the player's trigger / pitch / channel model.

## Decision

### Scope: one-shot, one sample per node

The v1.1 player is a **one-shot trigger sampler** — a trigger fires the sample from a start
offset to its end, no loop, no sustain-release. It covers drum hits, stabs, and FX, and is the
building block the groove-box Toy ([V1.3](../../ROADMAP.md)) assembles. Sustained/looped
playback (loop points, gate-release) and multisample zone-mapping are deliberately **out of
scope** — they become follow-on params / a higher-level Toy concern once this seam exists.

### Reference: a top-level `resources` table, addressed by logical id

The instrument document grows a `resources` table mapping a **logical id** to a source
(a file path today). A node refers to a resource by id, not by inline path:

```json
"resources": { "kick": "samples/kick.wav" },
"nodes": [ { "type": "sample", "address": "/kick", "sample": "kick" } ]
```

The indirection is the home the "Format & library" thread (resolution, naming, versioning)
will grow into, and it gives the loader **one list to resolve+decode** and a natural **dedup**
point: an id is decoded once, and every node referencing it shares the result.

Rejected — *per-node path string* (scatters the library concern across nodes) and *inline
base64 blobs* (bloat, unreadable diffs, doesn't scale past a demo).

### Ownership: a central `Arc<ResourceStore>` + `SampleId` handle — bank-ready, not operator-owned

Decoded audio lives in a **`ResourceStore`** built by the Coordinator (single-writer,
[ADR-0012](0012-boundary-and-threading.md)) and read **immutable** by Render. The operator
holds an `Arc<ResourceStore>` (shared; `spawn()` clones the `Arc`, cheap) plus a resolved
`SampleId`. Reads go through one accessor:

```
store.read(id, frame_range) -> &[f32]   // per channel; a pure fn of (id, range)
```

- **v1.1:** every resource is decoded up front and resident forever; `read` returns a slice of
  the resident buffer.
- **Future bank:** the *same* signature consults a warm-block cache; the operator is unchanged.

Rejected — *operator-owned `Arc<SampleBuffer>`* (the obvious minimal choice). It works for the
resident case and even shares across nodes if the loader hands out the same `Arc`, but it roots
the buffer in the operator, so the bank would have to re-plumb every reader. Rooting ownership
in a central store costs ~one struct now and is the seam streaming needs. The store also fits
ADR-0012 exactly: Coordinator builds it, Render only reads it.

**Determinism under streaming (settled now, so the seam is safe):** `read(id, range)` is a
**pure function** — it must always return the same floats for the same arguments. Warming is a
cache in front of that. If the warmer falls behind, the RT thread **underruns** (an xrun,
already the device layer's concern) — it must **never** substitute silence for not-yet-warmed
data, because that would make output depend on disk speed and break
[ADR-0001](0001-unified-block-graph-execution.md). Streaming thus affects *glitch-vs-no-glitch*,
never *values*. The invariant holds.

### Injection: a generic `bind_resources` trait hook, driven by a descriptor resource slot

The data reaches the type-erased operator through a **two-phase init** — the idiomatic Rust
pattern for a registry/plugin system once constructor injection is off the table (precedent:
nih-plug `initialize`, CLAP `activate`). The `Operator` trait gains one method, default no-op:

```rust
fn bind_resources(&mut self, store: &Arc<ResourceStore>, refs: &ResolvedRefs) {}
```

The loader calls it on each node after construction and before Plan fan-out; the sample player
grabs the `Arc` and resolves its id. `spawn()` carries both forward (struct fields read through
`&self`) while resetting per-Lane state. The **descriptor declares a resource slot** (named
`sample`), so the loader knows which nodes need a ref, the format can validate it, and the
schema / AI-grounding can express it ([ADR-0004](0004-ai-authorability-first-class.md)). The
hook stays generic (*resources*, not *samples*) — an embedder can author other resource-bearing
operators.

The load pipeline becomes: parse → build `Graph` → **resolve refs + decode → `ResourceStore`**
→ **`bind_resources` per node** → Instantiate (spawn fan-out).

Rejected — *downcast in the loader* (`op.downcast_mut::<SamplePlayer>()`): hardcodes a concrete
type into core, breaking the open registry. *Data-carrying constructor* (`fn(&LoadCtx) -> Box`):
changes every operator's signature for one operator's need.

### Trigger and pitch: downstream of the Voicer, exactly like the oscillator

The player slots into the **same seam as the oscillator** ([ADR-0010](0010-single-lane-operators.md)):
it sits downstream of a Voicer and reads the Voicer's per-Voice Signal outputs. Polyphony and
steal-oldest come **free** from the Voicer's Lane fan-out — the player re-implements none of it.

- in `freq` (Signal, optional) — pitch; in `gate` (Signal) — trigger.
- **Trigger = gate rising edge**, scanned per-sample (sample-accurate); the playhead starts at
  `start` on the exact edge frame.
- **One-shot plays to end, ignoring gate release** (fire-and-forget). Past the buffer end →
  silence, playhead parked until the next rising edge.
- **Retrigger on each rising edge** — a stolen/re-fired Voice restarts from `start`.
- **Pitch is latched at trigger**: `rate = (freq / hz(root)) · (file_sr / engine_sr)`, fixed
  for that hit. Live pitch-tracking (bend) is a deferred param.

Rejected — *own Message input + own expander* (duplicates Voicer allocation) and *bare Signal
trigger, no Voicer* (breaks the note→sound model every other rig uses).

### Sample-rate and interpolation: fold SR into the rate; linear interp

The file's native SR is **not** matched to the engine at load. Instead the ratio
`file_sr / engine_sr` is folded into the per-hit `f64` playback rate (above). A one-shot always
resamples for pitch, so there is no unity-pitch case a load-time resampler would protect — it
would be pure cost. Storing decoded-native also suits the bank (resample-on-read). Fractional
playhead positions use **linear interpolation** of the two straddling frames — RT-safe and
adequate; cubic/Hermite is a deferred quality param.

The playhead is a per-Lane `f64` cursor, persistent across `process` calls (like the
oscillator's phase), reset by `spawn()`. Keeping it `f64` and reads pure in `(id, cursor)`
keeps the whole path bank-streaming-safe.

### Channels: store all, select on read

`SampleBuffer` stores **every decoded channel** (planar). The player exposes a `channel` param:
`-1` = downmix (average all channels), `≥0` = pick that channel index (clamped to available).
This is useful permanently — pulling one side of a stereo recording, or mono-summing — and
survives into the multichannel-render era ([V1.5](../../ROADMAP.md)), where the player can also
read `io.lane()` to emit a Channel directly. Output is mono for now (the master and sink are
mono-Lane until V1.5), but no decode/store is wasted: the data model is already multichannel.

Rejected — *decode-and-downmix to mono* (throws away channels; forces a re-decode when stereo
lands).

### Decode location and format: core owns types + seam, native owns IO + WAV

Core stays portable and codec-free ([ADR-0012](0012-boundary-and-threading.md),
[ADR-0007](0007-osc-only-core.md) boundary-adapter pattern). It defines the data types
(`SampleBuffer`, `ResourceStore`, `SampleId`) and a **`ResourceResolver`** seam (id → bytes →
`SampleBuffer`). `reuben-native` provides concrete filesystem IO and a **WAV-only** decoder
(`hound`; PCM int + float — tiny, deterministic, no codec licensing). The resolver is
**injected at the load step** (`load_instrument(json, registry, resolver)`), so compressed
formats (symphonia) and non-file sources (bundle, network — the library thread) drop in behind
the same trait without touching core. Decode is **eager and non-RT**; RT `read()` only ever
touches decoded `f32`.

### Failure: degrade to silence with a warning; structural errors stay fatal

A missing sample or a bad decode **must not crash** a live instrument — but it is an authoring
error the user must see. Load errors therefore split into two tiers:

- **Structural / wiring errors stay fatal** (`LoadError`: unknown type, duplicate address,
  port-kind mismatch) — the graph cannot be built.
- **Resource errors become non-fatal warnings**: a node naming a missing id, a resolve failure,
  or a decode failure → the node binds an **empty (zero-length) `SampleBuffer`** (→ `process`
  outputs silence) and a **`LoadWarning`** is collected.

The load API becomes `load_instrument(...) -> Result<Loaded, LoadError>` where
`Loaded { graph, warnings: Vec<LoadWarning> }`. **Core returns structured warnings; native
surfaces them** (stderr / app log) — presentation stays at the boundary. Because the silent
node is a *reachable* state (not a defensive dead branch), it is a real, tested behavior.
`resources` entries no node uses are ignored. Paths resolve **relative to the instrument file's
directory** (a sample lives next to its rig); a configurable sample-root can come later.

Rejected — *hard-fail the whole load on any resource error*: consistent with the other
`LoadError`s, but a single missing file would take down an entire rig mid-performance.

## Consequences

- **`Operator` trait** gains `bind_resources(&mut self, &Arc<ResourceStore>, &ResolvedRefs)`,
  default no-op — the 12 existing operators are unaffected.
- **`Descriptor`** gains a way to declare a **resource slot** (drives loader validation, schema,
  AI-grounding). **`NodeDoc`** gains an optional `sample` (resource-id) field; **`InstrumentDoc`**
  gains a `resources` table.
- **Core** gains `SampleBuffer` (planar, all channels + native SR), `ResourceStore` (id →
  buffer; `read(id, range) -> &[f32]`, the bank-ready accessor), `SampleId`, a `ResourceResolver`
  seam, and `LoadWarning`. The load entry point becomes `load_instrument(json, registry,
  resolver) -> Result<Loaded, LoadError>`.
- **Native** gains a filesystem `ResourceResolver` and a `hound`-based WAV decoder; it surfaces
  `LoadWarning`s.
- **`Plan`** holds the `ResourceStore`; **Render** reads it immutable — no new RT machinery,
  no `Io` change (the `Arc` rides on the operator like a delay line). `tests/rt_safe.rs` extends
  to a sample-triggering rig: steady-state stays allocation-free.
- **New operator** `sample`: in `freq`, `gate`; out `audio`; params `root` (MIDI, default 60),
  `gain` (linear, 1.0), `start` (normalized 0..1, 0), `channel` (-1 mix / ≥0 pick). Registered in
  `Registry::builtin()`; schema regenerated.
- **Deferred behind these seams (not v1.1):** the streaming audio bank; loop/sustain sampler;
  multisample zones; compressed formats; cubic interpolation; live pitch-tracking; true
  multichannel output; a configurable sample-root.
- **Terminology:** *resource* = external data a node depends on (a sample today); *resource id*
  = its logical name in the `resources` table; *resident* = decoded-and-held-in-RAM (v1.1's only
  mode, as opposed to the future *streamed* bank.)
</content>
</invoke>

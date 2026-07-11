# ADR-0046: Coordinator & Swap: whole-Engine swap unit, mailbox install, box-transplant migration

## Status

Accepted (2026-07-11). The Coordinator/Swap design of the reuben MCP server effort —
wayfinder ticket [MCP/D (#274)](https://github.com/Impractical-Instruments/reuben/issues/274)
on map [#270](https://github.com/Impractical-Instruments/reuben/issues/270) — making
[ADR-0009](0009-graph-lifecycle.md)/[ADR-0012](0012-boundary-and-threading.md)'s design
vocabulary real. **Rides on** [ADR-0038](0038-interface-pipes-and-the-device-layer.md)
(device layer: §3 input-on-demand, §7 dark-degrade, §9 know-and-say),
[ADR-0039](0039-engine-in-core-embed-surface.md) (Engine as the shared embed surface), and
[ADR-0044](0044-mcp-stdio-sidecar.md) (stdio sidecar; the local structure channel it
delegated here). **Sharpens** [ADR-0045](0045-whole-document-edit-contract.md) §2's survivor
key (that ADR lands with MCP/C's branch; merge order is either-first, the reference resolves
once both are in). Feeds the swap-rudeness policy (MCP/G), the tool surface (MCP/E),
web-player parity (MCP/I), and the epic's M1/M2 tickets (MCP/J).

## Context

The gap #220 glossed over: `Engine` (`crates/reuben-core/src/engine.rs`) holds one Plan with
no install/replace method; the whole Engine moves into the cpal callback closure at
`audio::start` and is never reachable again; operator state lives inside the Plan's
`Box<dyn Operator>` instances (state and instance are one object — no extraction surface on
the trait); `reuben play` parks forever with no command path; the diagnostics counters are
log-only. Facts that bore on the design:

- The Plan is not self-contained at runtime: the Renderer's edge-buffer arena is sized to the
  Plan, and the Engine's scratch/channel counts are fixed at construction. Nothing smaller
  than that trio can run.
- The audio callback may never allocate, block, or free; all allocation lives in Swap's
  off-thread Instantiate (ADR-0009).
- `audio::start` opens the output stream once and an input stream only if the initial
  instrument binds input channels (ADR-0038 §3); the profile's output map is validated once
  against the initial engine's logical channel count. Opening a device is blocking I/O.
- The structure channel is request/response (swap must return a report; ADR-0044 requires a
  liveness probe), documents run to ~20KB (past safe UDP datagram size), and the workspace
  outside `reuben-mcp` is std-only, tokio-free (ADR-0044 §3).
- ADR-0044 §4 settled M1 control concurrency (last-write-wins) and delegated structure-edit
  arbitration here. ADR-0045 fixed the edit unit (whole document) and the survivor key's
  first two components (address + operator type).

## Decision

### 1. The swap unit is the whole Engine

What the Coordinator builds off-thread and the render side installs is a complete **Engine**
— Plan + Renderer + scratch, the Plan's runtime vessel. "Swap replaces the Plan" (ADR-0009)
stays the conceptual truth; mechanically the vessel crosses, so zero RT allocation holds by
construction: everything the callback touches post-install was built before install.

**Considered and rejected:** *a bare Plan* (the Renderer arena and Engine scratch must then
be rebuilt or resized RT-side — allocation on the audio thread, or a preallocated-maximum
bounds regime nothing else needs); *Plan + Renderer* (Engine scratch still resizes when
channel counts change — the same problem, smaller).

### 2. Handoff: two single-slot atomic mailboxes, one swap in flight

The RT boundary crossing is a pair of single-slot mailboxes (hand-rolled on `AtomicPtr`, in
core, no new dependency): an **install slot** the Coordinator fills and the callback drains,
and a **retire slot** the callback fills and the Coordinator drains. The Coordinator enforces
**one swap in flight** — it never installs the next Engine until the retired one came back —
and times out into an actionable error ("engine isn't consuming swaps; is audio running?")
rather than queueing blind. Reclaim is ADR-0009's deferred free: the Coordinator drops the
retired Engine on its own thread.

**Considered and rejected:** *a lock-free SPSC queue crate* (a new core dependency and a
queue depth implying multiple in-flight swaps the single writer never produces); *ArcSwap /
triple-buffering* (the callback needs `&mut Engine` — render mutates state — and reclaiming
the old side without RT drops fights the shape).

### 3. Install at the callback top; in-flight residue is discarded

The callback checks the install slot once, at the top, before any fill — a block boundary for
the device. The retiring Engine's already-rendered-but-unplayed scratch remainder (≤ 1 core
block, ~5ms) is discarded, and its pending control Messages are dropped: ADR-0045 §5 already
defines un-folded `send` tweaks as clobbered at Swap, and a typed Message minted against the
old Plan's port types may not be valid against the new one. Audible-rudeness policy
(fade/crossfade) is MCP/G's decision on top of this mechanism, which leaves it room: the
shell may briefly hold both Engines if MCP/G wants a crossfade. *(Resolved by
[ADR-0050](0050-swap-sonic-rudeness-ramp.md): a fixed master-gain ramp — begin the ramp at
the callback top, install at zero, ramp up; no second Engine is held.)*

**Considered and rejected:** *core-block-boundary precision* (mailbox awareness inside
`Engine::fill`'s hot loop, for a ≤5ms difference the rudeness policy papers over anyway);
*drain-then-swap* (preserves one last block of queued Messages against ADR-0045 §5's spirit,
for more RT branching).

### 4. Migration is box transplant via a precomputed table

Off-thread, the Coordinator matches its manifest of the installed Plan against the new Plan
and precomputes a **migration table** of (old index, new index) pairs. At install, the
callback executes a bounded loop of `mem::swap` over the matched nodes' operator boxes —
pointer swaps, no allocation, no drops. The displaced cold instances land in the retiring
Engine and free off-thread with it. The operator instance *is* the state: no new trait
surface on ~40 operators.

**Considered and rejected:** *a state-transfer API* (`extract`/`inject` on every operator —
a large hand-written surface bought now for cross-config migration nobody has asked for);
*serialize state through bytes* (a format per operator plus RT-side copying — maximum cost
for the same unused generality).

### 5. Survivor key: address + type + instantiate-time identity (sharpens ADR-0045 §2)

A transplanted box carries everything baked in at Instantiate: its `config` constants (a
voicer's `voices` pool size), its resolved resources (a sample player's decoded audio), and
hosted sub-plans built from referenced voice documents. Transplanting a box whose
instantiate-time inputs changed silently undoes the edit — bump `voices` 4→8 and the old
4-voice pool keeps playing; re-upload a sample at the same path (exactly MCP/F's flow) and
stale audio keeps sounding.

So a node is a **survivor** iff it matches on **fully-qualified address + operator type +
instantiate-time identity fingerprint**, where the fingerprint covers the node's normalized
`config` block plus the content identity of everything it resolved at Instantiate (resource
bytes, hosted sub-documents — recursively). Changed constant, changed resource content, or
changed hosted document = state reset for that node, exactly like a type change: it *is* a
different instantiation. ADR-0045's promise stands for everything else — rewired inputs,
changed params, new neighbors leave a survivor a survivor, because latches live in the Plan
(the new Plan's values win), not in the box. Fingerprint mechanism (content hash at load,
off-thread) is epic-level detail.

**Considered and rejected:** *address + type literally* (silently undoes config/resource
edits — the swapped document and the sound disagree, breaking "the document is durable
truth"); *config JSON without resolved content* (MCP/F's re-upload-same-path flow keeps
playing stale audio — a silent trap in the exact loop this epic builds).

### 6. Swap never touches devices; streams are fixed at `play` start

The install bundle carries everything per-Plan the callback consumes: the Engine plus a
freshly validated output map, rebuilt off-thread against the actual device channel counts. A
swapped-in instrument that binds input channels no open stream provides **dark-degrades** to
silence-fed pipes (ADR-0038 §7) with a loud warning in the swap report (§9 know-and-say).
Changing device topology means restarting `play` — documented, and consistent with the user
owning the engine (ADR-0044 §2). Coordinator-driven stream reopen stays a possible later
rung, not M2. (M1's restart-swap, §10, reopens streams as a side effect of its stop-the-world
shape — that tolerance is the interim's, not the contract's.)

**Considered and rejected:** *Coordinator reopens streams on demand* (an audible gap plus
stream-ownership complexity for a much bigger M2); *always open input at start* (directly
violates ADR-0038 §3's "an instrument without input pipes never touches an input device").

### 7. The Coordinator is a passive, OS-free `reuben_core::coordinator`

The Coordinator struct owns the Registry handle, the resolver, the installed-Plan manifest
(addresses + types + fingerprints + the canonical document, ADR-0012), and the mailbox
endpoints; it exposes roughly `swap_document(...) -> SwapReport` plus the retired-Engine
reclaim step. Its RT counterpart — the callback-side slot that checks the install mailbox,
runs the migration table, posts the retiree — also lives in core, embedded by each shell.
reuben-native contributes only what is native: the thread the Coordinator runs on and the
IPC that reaches it. The web shell (MCP/I) drives the same struct from its own boundary.
Core gains zero dependencies; single-writer discipline is enforced by `&mut self`.

**Considered and rejected:** *reuben-native placement* (the wasm web engine can't reach it —
MCP/I re-implements swap machinery); *a `reuben-coordinator` crate* (a boundary with nothing
to fence — ADR-0039/0044's twice-rejected shape).

### 8. The structure channel: loopback TCP, NDJSON, four verbs

The sidecar↔engine channel ADR-0044 delegated here is **TCP on 127.0.0.1** (loopback-only by
default — structure edits are more powerful than control, unlike OSC's 0.0.0.0:9000),
carrying **newline-delimited JSON**, one response per request in order. The server is a small
module in reuben-native (a thread in `play` owning the Coordinator); the client lives in
reuben-mcp. Zero new dependencies, cross-platform in std, netcat-debuggable. Default port and
override flag are epic-level detail. Four verbs:

- **`ping`** — liveness. This resolves ADR-0044's open probe mechanism, and is more honest
  than an OSC ping: it proves the structure channel itself.
- **`swap`** — install a document, accepted **by value or by path** (both resolver-loaded
  engine-side; which to expose is MCP/E's call — ADR-0045 §4 left both branches open).
  Returns a **SwapReport**: load errors (no install), warnings, and survivor/reset stats.
- **`get_document`** — the currently installed document (the Coordinator owns the canonical
  doc). A fresh conversation attaching to a running engine reads what's playing in one call;
  two-conversation workflows can sync.
- **`get_diagnostics`** — the ADR-0038 §9 counters, finally exposed past log-only.

**Considered and rejected:** *Unix socket / named pipe* (two platform code paths — AF_UNIX
isn't std on Windows — plus a socket-path discovery story, for no MVP gain); *an OSC
namespace* (violates ADR-0044's "structure does not ride OSC", breaks on >datagram documents,
no native request/response).

### 9. Concurrency: last-write-wins, with an opt-in `expect` guard

Structure edits from any number of clients are serialized by the Coordinator by construction;
the default arbitration is **last-write-wins**, extending ADR-0044 §4's control semantics.
Every SwapReport and `get_document` response carries the installed document's **content
hash**; a swap request MAY carry `expect` (the hash the client believes is installed), and a
mismatch rejects the swap with a conflict report naming the actual hash. One off-thread hash
compare — no sessions, no leases, no lock daemon. Whether tools surface `expect` is MCP/E's
call.

**Considered and rejected:** *pure LWW* (the lost-update scenario — conversation B silently
reverting A's work — becomes unguardable, documented or not); *mandatory optimistic
concurrency* (forces read-before-every-swap ceremony on the single-conversation majority
case).

### 10. Staging: M1 is restart-swap over the real channel

The full channel — all four verbs — lands in **M1**, with `swap` implemented as
**stop-the-world**: tear down the cpal streams, `Engine::from_document`, reopen. ~100ms gap,
every node cold — the known rudeness, documented — but zero core/RT changes (it lives
entirely in `play`'s channel thread), and it tolerates device-topology changes since streams
reopen. **M2 replaces only the machinery behind the same verb** (mailbox install, migration
table, fingerprints); the channel, protocol, and SwapReport contract carry forward unchanged,
the report gaining real survivor/reset stats. This widens the map's M1 line from
describe/validate/send to describe/validate/send/swap-rudely — recorded on the map.

**Considered and rejected:** *no swap until M2* (every structural change needs a human
restart — the conversational loop isn't conversational until the epic's hardest milestone);
*file-watch reload* (validation errors land in `play`'s stderr where the agent can't see
them, and it's a second structure path M2 throws away).

## Consequences

- ADR-0009's "atomically install it at a block boundary" is mechanically pinned: the vessel
  is the Engine, the boundary is the callback top, migration is a bounded pointer-swap loop,
  reclaim is a mailbox return. Render-side swap cost: one atomic check per callback, plus the
  table walk on the callback that installs.
- ADR-0045 §2's survivor key gains instantiate-time identity: rename, type change, config
  change, or resolved-content change = state reset; everything else survives. Authoring
  guidance inherits a third rule of thumb: *changing what a node was built from resets it*.
- ADR-0044's open liveness-probe mechanism is resolved (`ping` on the structure channel), and
  its M1 engine-side work item list concretizes: the channel module + thread, restart-swap,
  and the four verbs, all in `reuben play`'s process.
- The epic's M2 engine tickets fall out of §§1–7: the mailbox pair, `reuben_core::coordinator`
  + manifest/fingerprint, the migration table, the RT-side slot in the shells, and the
  SwapReport upgrade. MCP/G designs audible rudeness on top of §3's mechanism (which can hold
  two Engines briefly); MCP/I reuses §7's seam from the worklet boundary.
- Multi-conversation workflows get an opt-in stale-swap guard riding the content hash;
  single-conversation flows pay nothing.
- reuben-core gains a coordinator module and zero dependencies; tokio stays fenced in
  reuben-mcp (ADR-0044 §3); the OSC control plane is untouched.

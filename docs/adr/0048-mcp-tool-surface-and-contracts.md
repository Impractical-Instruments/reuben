# ADR-0048: MCP tool surface and contracts

> **§7 amended by [ADR-0059](0059-cross-lane-grounding-unification.md).** The resource surface
> changes: `reuben://schema/instrument` is **deleted** — the instrument JSON Schema has no
> grounding role in any lane, and its one real job (the registry guard) is same-commit
> native≡wasm describe parity instead (ADR-0059 §4) — while the vocabulary view and the
> generated library index **join** the set (ADR-0059 §3/§6). The guide resource, the
> no-prompts posture, and everything else in §7 stand.

> Renumbered from 0047 (2026-07-11) — that number was already held by
> [0047-normalization-is-a-type.md](0047-normalization-is-a-type.md). External references dated
> 2026-07-10/11 (map [#270](https://github.com/Impractical-Instruments/reuben/issues/270), ticket
> [MCP/E #275](https://github.com/Impractical-Instruments/reuben/issues/275)) may still say
> ADR-0047 for this document.

## Status

Accepted (2026-07-11). The tool-surface decision of the reuben MCP server effort — wayfinder
ticket [MCP/E (#275)](https://github.com/Impractical-Instruments/reuben/issues/275) on map
[#270](https://github.com/Impractical-Instruments/reuben/issues/270), pinning the contract
layer of [#220](https://github.com/Impractical-Instruments/reuben/issues/220)'s sketched
surface — grounded on the [MCP/A landscape research](../research/mcp-rust-server-landscape.md)
([#271](https://github.com/Impractical-Instruments/reuben/issues/271)). **Rides on**
[ADR-0020](0020-introspection-and-patcher-skill.md) (describe/validate as pure library
functions; the loader as the single validation authority),
[ADR-0036](0036-instrument-library-and-format-versioning.md) (format versioning: absent
means 1, only the future refuses), [ADR-0044](0044-mcp-stdio-sidecar.md) (stdio sidecar,
user-owned engine, fail-fast engine tools),
[ADR-0045](0045-whole-document-edit-contract.md) (whole-document edit contract; doc transport
handed here by its §4), and [ADR-0046](0046-coordinator-swap-engine-unit.md) (Coordinator &
Swap: the structure channel and its four verbs, restart-swap in M1, the sharpened survivor
key — which handed here three exposure calls: swap by-value-or-path, the `expect` guard, and
`get_document`). **Amends [ADR-0038](0038-interface-pipes-and-the-device-layer.md) §9**: the
deferred diagnostics endpoint is the MCP `get_diagnostics` tool over the structure channel,
not an OSC endpoint. Feeds skills↔server single-sourcing (MCP/H) and the implementation epic.

## Context

- [#220](https://github.com/Impractical-Instruments/reuben/issues/220) sketched seven tools:
  `describe_operators`, `describe_operator(name)`, `describe_instrument`, `validate`, `swap`,
  `send`, `get_diagnostics`. The pure internals exist (`cli.rs`, descending to
  `reuben_core::introspect` per ADR-0044 §3): `OperatorInfo`/`PortInfo`, `PatchBoundary`,
  `ValidateReport`/`Diag`; `Engine::queue_osc` *is* send.
- The process model is fixed (ADR-0044): pure tools always available in the sidecar; engine
  tools reach a user-owned `reuben play` — OSC for control, the loopback TCP/NDJSON
  **structure channel** (ADR-0046 §8: `ping`/`swap`/`get_document`/`get_diagnostics`) for
  everything else. All four verbs land in **M1**, with `swap` as restart-swap; M2 replaces
  the machinery behind the same verb (ADR-0046 §10).
- The edit contract is fixed (ADR-0045): whole document in, report out; no edit-command
  surface exists or is reserved; `send` is ephemeral audition, the document is durable truth.
  Its §4 handed one input here: document **by value vs by path**. `FsResolver` roots at the
  instrument file's own directory (sibling-first, library-root fallback) — a by-value document
  has no natural anchor.
- MCP structured tool output (`outputSchema`/`structuredContent`) has been in the spec since
  2025-06-18 and rmcp derives both via schemars. A tool call distinguishes protocol errors
  (JSON-RPC), execution failures (`isError: true`), and ordinary results.
- The diagnostics counters exist (`diagnostics.rs`, the designated *one* counter surface) but
  the `Arc<Diagnostics>` never escapes `audio::start`; ADR-0038 §9 deferred "an OSC diagnostic
  endpoint" as explicitly later.

## Decision

### 1. The roster: eight tools, all M1; milestones change machinery, never surface

`describe_operators`, `describe_instrument`, `validate`, `send`, `engine_status`, `swap`,
`get_current_instrument`, `get_diagnostics` — all shipped in M1. M2 upgrades what stands
behind `swap` (mailbox install and box-transplant migration instead of restart-swap,
ADR-0046 §10) without touching any tool's name, schema, or result shape.

- `describe_operators` **merges** #220's `describe_operators`/`describe_operator`: one tool
  with an optional `name` filter, mirroring `introspect::describe(Option<&str>)` exactly.
- `engine_status` is new: the liveness probe (ADR-0046 §8's channel `ping`) exposed as a
  tool — near-zero marginal cost, and agents get a deliberate "are we live?" check before an
  audition loop.
- `get_current_instrument` is new: ADR-0046 §8's `get_document` verb exposed as a tool
  (see §5) — a fresh conversation attaches to a running engine in one call, and the
  multi-client blind spot (another conversation swapped the engine under you, ADR-0044 §4)
  becomes observable.
- **No edit-command tool names exist or are reserved** (ADR-0045 §§1,3).

**Considered and rejected:** a separate `describe_operator` zoom tool (roster noise for a
parameter's worth of difference); staging `swap`/`get_diagnostics` to M2 (the structure
channel ships in M1 — ADR-0046 §10's restart-swap exists precisely so the conversational
loop is conversational from the first milestone); leaving `get_document` unexposed (the
seam's blocker — what the Coordinator retains — was resolved by ADR-0046 §7: it owns the
canonical document).

### 2. Document transport: read-only tools take path or value; swap is path-only

`validate` and `describe_instrument` accept **exactly one of** `path` (string; resolver rooted
at the file's directory, sibling-first, library-root fallback) or `document` (inline JSON
object; optional `resolve_from` directory anchors nested references, defaulting to the
sidecar's cwd). `swap` accepts **`path` only** — the channel verb takes both (ADR-0046 §8)
and the exposure call was this ADR's: the tool exposes the path branch.

Path is the low-token default for the MVP persona (a dev with a checkout); inline `document`
keeps try-before-write validation loops off the filesystem. But you can only **install** what
exists on disk: a by-value swap would let the playing instrument exist only in conversation
context, dying with the chat — making ADR-0045 §5's "the document is durable truth" quietly
false.

**Considered and rejected:** by-value `swap` (above); path-only everywhere (a candidate doc
must hit disk before it can even be validated); inline-only (ignores the persona and pays the
full document in tokens on every call).

### 3. Error layers: a failed validation is a successful tool call

Three layers, used distinctly:

- **Protocol (JSON-RPC) errors**: malformed calls — rmcp's input-schema validation.
- **`isError: true`**: the tool **could not do its job** — unreadable path, missing/ambiguous
  one-of, unknown operator name, engine unreachable (the message carries the "start
  `reuben play`" guidance, ADR-0044 §2).
- **Ordinary results** carry the deliverable — *including* `{ok: false}` reports: a report
  naming the offending node is the tool *working*. A rejected swap (validation failure or
  `expect` conflict, §5) is the guard guarding, not the tool failing.

Models treat `isError` as "the call failed, back off / retry differently" — exactly wrong for
a diagnostic report they should act on. Corollary: `describe_instrument` on a document that
fails to load is `isError` (there is no boundary to describe; the message directs to
`validate` for the full report).

Every tool declares an `outputSchema` and returns `structuredContent` plus a human-readable
text block.

**Considered and rejected:** `isError` for any not-ok outcome (conflates "your document is
broken" with "the tool is broken"); text-only results (the model re-parses JSON out of prose).

### 4. The report schema: Diag everywhere, warnings included; no version field

```
Report = { ok: bool, errors: Diag[], warnings: Diag[] }
Diag   = { node?: string, port?: string, message: string }
```

- Warnings are **promoted** from today's bare strings to `Diag`: `LoadWarning` already carries
  the offending node (`MissingResource { node, slot, id }`), so the model jumps to the node
  for a warning exactly as for an error. (A small mapping addition in introspect; the CLI's
  `validate --json` output changes shape with it.) ADR-0046 §6's dark-degrade warning on swap
  lands in the same channel.
- `format_version` surfaces as an **ordinary error Diag**: `LoadError::UnsupportedVersion`'s
  message already names both versions and the remedy (ADR-0036 §4). Absent→1 normalization
  stays invisible by design. No dedicated version field in reports; the sidecar's supported
  `format_version` lives in `engine_status`.

**Considered and rejected:** warnings as strings (drops localization the core already has); a
dedicated `{document_version, supported_version}` response field (redundant on every happy
path for a once-per-format-break event).

### 5. Per-tool contracts

| Tool | Input | Success output |
|---|---|---|
| `describe_operators` | `{ name? }` | `{ operators: OperatorInfo[] }` |
| `describe_instrument` | one-of `path`/`document`, `resolve_from?` | `PatchBoundary` |
| `validate` | one-of `path`/`document`, `resolve_from?` | `Report` |
| `send` | `{ messages: [{address, args}] }` (min 1) | `{ sent: N }` |
| `engine_status` | — | `{ reachable, endpoints, sidecar }` |
| `swap` | `{ path, expect? }` | `Report` + `content_hash` + `diff` |
| `get_current_instrument` | — | `{ document, content_hash }` |
| `get_diagnostics` | — | the four counters |

- **`describe_operators`**`({name?})` → `{operators: OperatorInfo[]}`. `OperatorInfo`/
  `PortInfo` serialize as today (ADR-0020's agent-grounding shapes). Unknown `name` ⇒
  `isError`.
- **`describe_instrument`**`({path?|document?, resolve_from?})` → `PatchBoundary
  {instrument, inputs: PortInfo[], outputs: PortInfo[], dark_inputs: string[], dark_outputs:
  string[], warnings: Diag[]}`. Always describes the document handed to it — for what the
  engine is *playing*, use `get_current_instrument`.
- **`validate`**`({path?|document?, resolve_from?})` → `Report`. The engine's own load +
  instantiate path (ADR-0020), stat-only resources.
- **`send`**`({messages: [{address: string, args: (number|string)[]}]})` → `{sent: N}`.
  **Batch**, because the natural authoring gesture is multi-control ("cutoff down, resonance
  up") and round-trips are a conversational agent's expensive unit; sequential datagrams,
  the semantics of a physical controller twiddling two knobs. Probe-first: engine unreachable
  ⇒ `isError`. The ack means "engine alive + datagrams dispatched" — UDP promises no delivery
  and no application receipt, documented honestly. The tool description carries ADR-0045 §5:
  `send` is ephemeral audition, clobbered at the next Swap; fold kept values into the document.
- **`engine_status`**`({})` → `{reachable: bool, endpoints: {structure: string, osc: string},
  sidecar: {version: string, format_version: int}, guidance?: string}`. The probe is the
  structure channel's `ping` (ADR-0046 §8 — it proves the channel itself, which `send`'s OSC
  path does not). **Never `isError` for a dead engine** — answering "reachable?" *is* its
  job; `guidance` ("start `reuben play`…") appears when unreachable. Engine-side identity
  (engine version, loaded instrument name) stays out of this response — what's loaded is
  `get_current_instrument`'s job; the response can grow an `engine: {…}` field additively if
  `ping` learns to carry identity.
- **`swap`**`({path, expect?})` → `Report` plus `content_hash` (the installed document's
  hash, ADR-0046 §9) plus, on success, a **diff summary**: `{survived: int, state_reset:
  string[], added: string[], removed: string[]}`. `state_reset` = addresses present in both
  documents whose node did **not** survive — a type change *or* an instantiate-time
  fingerprint change (config, resolved resource content, hosted sub-document; ADR-0046 §5) —
  *announced* rather than discovered by ear. `added`/`removed` catch whole-document
  re-emission accidents: a param tweak that reports `removed: ["voice1"]` is a typo'd address
  caught while still fixable. `ok: false` ⇒ nothing installed; the old sound keeps playing.
  **`expect`** (optional) is ADR-0046 §9's opt-in guard: the content hash the client believes
  is installed; a mismatch returns `{ok: false, conflict: {expected, actual}}` — no install,
  the model re-reads via `get_current_instrument` and reconciles. In **M1** the verb is
  restart-swap (ADR-0046 §10): the description documents the ~100ms gap and every-node-cold
  honestly, and the diff reports `survived: 0`; M2 fills in real survivor stats behind the
  unchanged shape.
- **`get_current_instrument`**`({})` → `{document, content_hash}` — the Coordinator's
  canonical installed document (ADR-0046 §§7–8). Engine unreachable ⇒ `isError`.
- **`get_diagnostics`**`({})` → `{output_xruns, input_ring_underruns, input_ring_overruns,
  input_ring_producer_drops}` — running totals since engine start; xruns count events, ring
  counters count **frames**. Engine unreachable ⇒ `isError`. New counters land as new fields
  (`diagnostics.rs` stays the one counter surface).

### 6. `get_diagnostics` is the ADR-0038 §9 endpoint — vehicle amended

ADR-0038 §9 deferred "an OSC diagnostic endpoint explicitly later." That endpoint is
`get_diagnostics` over the structure channel (ADR-0046 §8), shipping in M1; the OSC vehicle
is superseded — ADR-0044 already ruled structure and diagnostics off OSC. The counters and
the know-and-say policy are unchanged. Engine-side plumbing is an epic work item:
`audio::start` exposes its `Arc<Diagnostics>` to `play`'s channel thread.

**Considered and rejected:** an OSC diagnostics query (contradicts ADR-0044's channel split);
computed health verdicts in the response (the model can read four counters; policy stays out
of the engine, ADR-0038 §9).

### 7. Resources ship; prompts do not (M1)

The server declares the **resources** capability with a small static set — no
subscribe/listChanged:

- `reuben://schema/instrument` — the generated `crates/reuben-core/schema/instrument.schema.json`
- `reuben://guide/authoring` — `docs/agents/authoring.md`

**No prompts capability in M1**: MCP prompts surface as user-invoked slash commands, the
authoring workflow already lives in the repo skills, and slash-command duplication is exactly
the drift MCP/H exists to referee. The server `instructions` field carries the one-paragraph
workflow gist (the document is truth; `send` to try, doc-edit + `swap` to keep; start
`reuben play` first). *What content* single-sources these resources against the skills is
MCP/H's question — this ADR fixes only the surface.

**Considered and rejected:** tools-only (agents can't `@`-mention or list the schema/guide —
the research memo's rule of thumb is resources for stable browsable documents, tools for
anything computed); prompts in M1 (instantly two sources of truth before MCP/H decides).

### 8. Obligations and seams

- **SwapReport ↔ tool report**: ADR-0046 §8's channel `SwapReport` (load errors, warnings,
  survivor/reset stats, content hash) and §5's tool response are the same data; the tool
  layer's field names in this ADR are the MCP contract, and the epic keeps the channel and
  tool shapes from drifting (ideally one serde type).
- **Liveness probe**: resolved — `ping` on the structure channel (ADR-0046 §8);
  `engine_status` wraps it.
- **Diagnostics plumbing** (epic): `Arc<Diagnostics>` exposure per §6.
- **Content single-sourcing** (MCP/H): the two resource URIs + `instructions` text vs the
  patcher/authoring skills.

## Consequences

- The epic's M1 tickets implement eight tools, two resources, and server instructions against
  pinned schemas; the M2 engine tickets change what stands behind `swap` without touching the
  surface (the diff summary's `survived` simply starts telling the truth about migration).
- Introspect's report types change once: `ValidateReport.warnings` and
  `PatchBoundary.warnings` become `Diag[]` (localized); the CLI's JSON output shape moves
  with them.
- ADR-0038 §9's "OSC diagnostic endpoint later" is superseded by `get_diagnostics`; nothing
  about the counters or policy changes.
- ADR-0046's three handoffs are closed: the swap tool exposes the path branch, `expect` is
  surfaced (opt-in), `get_document` is exposed as `get_current_instrument`.
- MCP/H starts from a fixed surface: two resource URIs, server `instructions`, and tool
  descriptions carrying try-then-commit.
- The serde shapes named here *are* the contract: rmcp derives the declared `outputSchema`s
  from the introspect types via schemars, so contract drift is a compile-time concern, not a
  documentation one.

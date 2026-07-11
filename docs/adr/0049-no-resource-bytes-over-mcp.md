# ADR-0049: No resource bytes over MCP — samples ride the filesystem convention

## Status

Accepted (2026-07-11). The sample/resource-upload decision of the reuben MCP server effort —
wayfinder ticket [MCP/F (#276)](https://github.com/Impractical-Instruments/reuben/issues/276)
on map [#270](https://github.com/Impractical-Instruments/reuben/issues/270) — closing
[#220](https://github.com/Impractical-Instruments/reuben/issues/220)'s open question 4
("does the surface accept resource bytes?"). **Rides on**
[ADR-0016](0016-sample-player-and-resource-store.md) (the `resources` table, the
`ResourceResolver` seam, resource failures as non-fatal warnings),
[ADR-0036](0036-instrument-library-and-format-versioning.md) (sibling-first resolution — a
sample lives next to its rig), [ADR-0044](0044-mcp-stdio-sidecar.md) (stdio sidecar; MVP
persona is a dev with a checkout), and
[ADR-0048](0048-mcp-tool-surface-and-contracts.md) (the fixed eight-tool surface with **no
save/write tool** — the agent edits documents with its own file tools). Hands the browser
resource-delivery seam to [MCP/I (#279)](https://github.com/Impractical-Instruments/reuben/issues/279)
and the authoring-guide content obligation to
[MCP/H (#278)](https://github.com/Impractical-Instruments/reuben/issues/278).

## Context

- "Use this sample" was the one authoring gesture the tool surface hadn't settled: an
  instrument references decoded audio through the document's `resources` table (logical id →
  source; ADR-0016), the filesystem resolver finds it sibling-first with a library-root
  fallback (ADR-0036), and a missing or undecodable resource is a **non-fatal, node-localized
  warning** (silence + `Diag`, ADR-0048 §4) — never a crash.
- The charter put upload **in scope to decide**, with "decide to defer" a legitimate
  resolution as long as it was decided deliberately.
- The transport fact that frames everything: MCP's only client→server byte path is **base64
  in a tool argument**, which flows through the model's context window. A five-second stereo
  WAV is ~1.3 MB of base64; a real sample library is hopeless. The protocol has no
  out-of-band binary push.
- ADR-0048 fixed the surface at eight tools and deliberately shipped **no server-side write
  path**: documents are edited by the agent's own file tools, and `swap` is path-only because
  durable truth lives on disk.

## Decision

### 1. No upload tool — "use this sample" is a filesystem gesture

M1 ships no byte-accepting tool. The agent driving the server writes (copies, moves, or
synthesizes) the WAV **next to the instrument** with its own file tools, adds a `resources`
entry, and references it by relative path — sibling-first resolution makes that the blessed
location (ADR-0036 §3). This is the same posture ADR-0048 fixed for documents: no server-side
write path, symmetric for resources. The loop self-corrects without new machinery: `validate`
stats the file and reports a missing one as a node-localized warning; an undecodable file
surfaces in `swap`'s report as the dark-degrade warning — announced, not discovered by ear.

**Considered and rejected:** an `upload_sample` tool taking base64 — it is a file-write tool
in a costume, reintroducing exactly the server-side write path ADR-0048 ruled out, and its
transport (base64 through model context) makes it useless beyond toy clips anyway.

### 2. None scheduled — the revive trigger is a persona change, not a milestone

Byte-upload gets **no later-milestone ticket** in the implementation epic. What would revive
it is not time passing but the *persona* changing: a packaged, non-dev client without file
tools — and packaging/distribution for non-devs is already out of scope by charter. If that
persona line is ever redrawn, byte-upload returns as part of **that** effort, with its
transport question (base64 ceilings, chunking, formats) answered in its real context.
#220's open question 4 closes as **no**.

**Considered and rejected:** a scheduled M3 byte-upload ticket — it would pin today's guesses
to a persona the charter has explicitly ruled out.

### 3. The browser half is a delivery seam, not an upload tool — named for MCP/I

For the browser-embedded engine (`reuben-web`), "does MCP accept bytes" was never the real
question: that engine has no filesystem, and its resources are served by core's
`MemoryResolver` (exact-key) behind the same `ResourceResolver` seam, staged fetch-on-miss by
the page (ADR-0040). The question transforms into **resource delivery** — how sample bytes
reach a `MemoryResolver`-backed engine — and that is part of MCP/I's parity architecture
(fetch-by-URL into the page, the authoring surface pushing into the worklet, or whatever MCP/I
weighs). MCP/I's ticket body grows a candidate bullet naming the seam so it survives this
ticket's closure.

### 4. M1's one obligation: the sample workflow is required authoring-guide content

The path story ships as **guidance, not surface**. The authoring guide
(`docs/agents/authoring.md`, served as `reuben://guide/authoring` per ADR-0048 §7) must carry
the sample workflow: the placement convention (sample next to the instrument, `resources`
entry by logical id, relative path), the agent-writes-bytes-itself pattern, and the degrade
behavior (missing = silence + localized warning, so a wrong path is diagnosed from the report,
not by ear). *What content* single-sources this against the skills is MCP/H's question — this
ADR fixes only that the content must exist.

**Considered and rejected:** an opt-in full-decode mode on `validate` (catch a corrupt WAV
before `swap` announces it) — no client needs it yet; `validate` stays stat-only (ADR-0048 §5)
and the `swap` report already announces decode degrades.

## Consequences

- The implementation epic carries **no resource-upload ticket**; the M1 sample story is
  documentation (MCP/H's content pass), not code.
- MCP/I inherits a named seam: resource delivery to the filesystem-less browser engine.
- The map's provenance-stamping fog narrows: with no upload flow, MCP/F surfaces no
  server-side hook for stamping library-bound instruments; the epic assembly ticket (MCP/J) is
  the sole remaining candidate, else provenance falls to the curated-library effort's lane.
- A future packaged-app effort that needs byte transport starts from this ADR's rejection
  rationale rather than from silence.

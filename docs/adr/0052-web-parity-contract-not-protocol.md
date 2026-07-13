# ADR-0052: Web-player parity: the contract ports, not the protocol — no browser MCP

> **Amended by [ADR-0056](0056-web-product-extracted-to-private-repo.md).** The in-page tool layer
> of §2 and the by-value `swap` of §3 are private code now. What remains binding on **this** repo is
> §5: the contract types live OS-free in `reuben-core` and one schema serves many doors — that is
> precisely the invariant the extraction depends on, since the private repo generates its tool
> schemas from those types across the submodule boundary. The no-browser-MCP conclusion stands; the
> code that honors it is elsewhere.

## Status

Accepted (2026-07-11). The web-player-parity decision of the reuben MCP server effort —
wayfinder ticket [MCP/I (#279)](https://github.com/Impractical-Instruments/reuben/issues/279)
on map [#270](https://github.com/Impractical-Instruments/reuben/issues/270), resolving the
charter's "web-player parity in scope" as a **conscious deferral with a fully named
architecture**. **Rides on** [ADR-0040](0040-raw-c-abi-worklet-boundary.md) (the C-ABI
worklet boundary and its staging lifecycle), [ADR-0042](0042-share-links.md) (share links:
the bundle codec and its WAV trust boundary), [ADR-0044](0044-mcp-stdio-sidecar.md)
(introspection descends to `reuben_core::introspect` — wasm-reachable by construction),
[ADR-0045](0045-whole-document-edit-contract.md) (whole-document edit contract),
[ADR-0046](0046-coordinator-swap-engine-unit.md) (Coordinator and its RT slot live in core
so "the web shell drives the same struct from its own boundary", §7),
[ADR-0048](0048-mcp-tool-surface-and-contracts.md) (the eight tool contracts this ADR
declares portable), and [ADR-0049](0049-no-resource-bytes-over-mcp.md) (which handed the
browser resource-delivery seam here, §3). Hands one type-placement constraint to the epic
assembly (MCP/J); the future **web-chat effort** starts from this ADR.

## Context

- The ticket asked how the browser-embedded engine (`reuben-web`, a C-ABI cdylib in an
  AudioWorklet) gets the conversational authoring surface. The candidates on the table all
  assumed a **desktop MCP client driving a browser tab** — an OSC-over-WebSocket door, a
  native relay presenting the tab as a remote engine, or authoring tools compiled into the
  bundle for a desktop agent to call.
- Grilling surfaced that the real web authoring future is different in kind: **a chat window
  in the web app itself**, patching the instrument playing in that very page. No desktop
  process is in that loop at all. The desktop→tab candidates answer a persona that doesn't
  exist: every concrete use (author a Toy, hear it in the target browser) works as *author
  natively against `reuben play`, open the tab to spot-check* — no conversational machinery.
- The transport facts are one-sided. A browser tab cannot listen — it only dials out — so
  ADR-0046's shape (sidecar dials a server the engine hosts) cannot be copied; something
  would have to invert, and every inversion adds a process or couples the sidecar to a
  browser transport. Meanwhile the surveyed audio-MCP landscape (the MCP/A memo) has no
  browser-relay precedent.
- The pieces for the in-page future are already positioned, by decisions made for other
  reasons: `describe`/`validate` live in `reuben_core::introspect` (OS-free, wasm-reachable;
  ADR-0044 §3); the Coordinator and its RT-side slot live in core precisely so the web shell
  can drive them (ADR-0046 §7); the page already does teardown-rebuild per Toy — restart-swap
  semantics in the flesh; `send` exists as the flat control codec (`queue_control`); resource
  bytes reach the engine only via `stage_resource` (fetch-on-miss staging, ADR-0040); and the
  player ships a **keep gesture** today — the Share button minting a self-contained bundle
  URL (ADR-0042), samples refused at mint by its trust boundary.
- One native rule cannot port literally: "durable truth lives on disk" (ADR-0048 §2's
  path-only `swap`). A tab has no disk, and the wasm engine can never read browser storage
  directly — IndexedDB is an async JS-side API, invisible from the worklet — so *any*
  persistence layer feeds the engine by value.

## Decision

### 1. No MCP on the web lane — the contract ports, not the protocol

Web parity is **not** "MCP reaches the tab." What ports to the browser is **ADR-0048's tool
contracts** — the names, input/report shapes, and error-layer discipline ("a failed
validation is a successful call") — carried by an **in-page tool layer** a future chat agent
binds directly. MCP-the-protocol stays native-only. The durable artifacts of this effort are
the contracts and the structure-channel machinery; MCP is one thin door over them (native
conversation clients), the in-page layer is the second. No verb means different things behind
different doors.

**Considered and rejected — the desktop→tab bridge, outright** (not dormant): a long-lived
native relay presenting the connected tab as a structure-channel engine (a new process and a
browser transport to maintain, for a loop with no persona behind it); a WebSocket listener
mode in `reuben-mcp` (the rendezvous dies with each conversation, concurrent conversations
fight over the port, and the fenced dependency tree grows a network stack); wasm authoring
exports *for a desktop agent* (the dev persona already has describe/validate native in the
sidecar). The **revive trigger is a persona change** — someone authoring from a desktop
conversation against sound that must come out of a browser — not a milestone. The dev
spot-check loop stays manual and machinery-free: author against `reuben play`, open the tab.

### 2. The in-page tool layer: eight contracts, in-page anchors

The future web-chat effort builds a JS tool layer over the existing C-ABI, one tool per
ADR-0048 contract, same report shapes (`Report`/`Diag`, diff summary, `content_hash`):

| Contract | In-page anchor |
|---|---|
| `describe_operators` | **new wasm export** over `reuben_core::introspect` |
| `describe_instrument` | **new wasm export** over `reuben_core::introspect` |
| `validate` | **new wasm export** over `reuben_core::introspect` (staged-resource stat) |
| `send` | existing flat control codec (`queue_control`) |
| `swap` | the existing Toy-switch path: destroy → stage → construct (restart-swap) |
| `engine_status` | trivial — the page *is* the engine host; reports worklet/audio state |
| `get_current_instrument` | page state — the shell owns the staged document |
| `get_diagnostics` | **named seam**: the web shell grows counters when the chat lands |

Web `swap` is restart-swap indefinitely: the worklet is single-threaded, so M2's off-thread
Engine build + mailbox install does not port (wasm threads/SAB are a later rung nobody has
asked for), and the construct-on-audio-thread gap is already the accepted Toy-switch shape.
M2 gifts the web lane nothing behind `swap` — expected, documented, and fine behind an
unchanged contract (`survived: 0`, the same honesty as native M1).

**Considered and rejected:** designing the chat window, its agent host (client-side API key
vs backend proxy — auth, cost, product questions), or the tool layer's build tickets here —
that is the web-chat effort's lane; this ADR fixes the architecture it starts from.

### 3. The one contract divergence: in-page `swap` is by-value; truth delegates to the keep gesture

Native `swap` is path-only because the engine-side resolver reads the filesystem — the
mechanism that makes "durable truth lives on disk" enforceable. In the browser that mechanism
does not exist: even a full browser database would be read by the page and handed to the
engine **by value**. So the in-page `swap` takes the document by value — the composition
primitive any future persistence sits on — and the invariant restates one level up:
**every kept swap pairs with a keep gesture**. The Share link is that gesture today
(sample-free bundles, ADR-0042); the library save/load rung (#151's named seam) covers the
rest when it exists. This lands on the web-chat effort as an **ordering constraint**: the
chat window does not ship to users before a keep gesture is wired into its loop. The in-page
contract keeps `content_hash`, so a future store gets expect-guards and dedup-by-hash with no
contract change.

**Considered and rejected:** *staged-key-only swap* (staged memory is exactly as volatile as
page memory — ceremony pretending to be durability); *persist-before-swap enforcement*
(hard-couples the tool layer to a persistence design that belongs to the player/product
effort); *building the browser database first as a prerequisite* (nothing in the layer's
design depends on its schema, and by-value is what it will compose with anyway).

### 4. Resource delivery: staging is the seam (closes ADR-0049 §3's handoff)

Bytes reach the browser engine **only** via the existing staging lifecycle
(`stage_resource`, fetch-on-miss discovery) — that *is* the resource-delivery seam ADR-0049
handed here. Byte sources are **page gestures**: fetch from the asset base today; a file
picker / drag-drop when the chat effort needs user samples. ADR-0049's posture holds in-page:
no resource bytes ride the agent's context on this lane either — the agent references by
key; the page moves the bytes.

### 5. One constraint on the epic: contract types live OS-free in core

The serde types the contracts are made of — `Report`/`Diag`, the diff summary, `SwapReport`,
`content_hash` — live in **`reuben-core`**, not `reuben-native` or `reuben-mcp`, so the wasm
lane reuses the exact types the native lane serializes (one schema, two doors). This was
already implied (ADR-0046 §7 put the Coordinator that produces `SwapReport` in core;
ADR-0048 §8 wants channel and tool shapes as one serde type); it is now explicit and binds
the epic's M1 tickets via the assembly (MCP/J).

## Consequences

- The epic's build scope is unchanged: **M1 + M2, native-only**. Web parity contributes no
  build tickets — only §5's type-placement constraint, carried by MCP/J.
- The future web-chat effort starts from a named architecture instead of a blank page: the
  in-page tool layer (§2), the by-value + keep-gesture contract (§3), the staging seam (§4),
  and an explicit non-goal (no browser MCP, §1).
- The MCP server's role is sharpened, not shrunk: it remains the native front door
  (client reach beyond Bash-capable agents, schemas derived from the shared types, the
  resources surface), and it is the cheapest layer in the stack to swap — nothing beneath it
  is MCP-shaped.
- ADR-0049's open browser seam is closed; ADR-0048's contracts acquire a second consumer,
  which is exactly the pressure that keeps them honest.
- Nobody builds a bridge process by accident: the desktop→tab loop is rejected on the record,
  with its revive trigger named.

# Why: The user owns the engine: the sidecar never spawns or kills reuben play, engine-touching tools fail fast with actionable guidance, and multiple clients are tolerated rather than arbitrated.

[Rule](../../agent-mcp.md#user-owned-engine)

Because the sidecar is disposable ([mcp-stdio-sidecar](mcp-stdio-sidecar.md)), *something* durable
has to own the sound, and it is the user, not the shim. The shim never spawns, kills, or restarts
`reuben play`. Auto-spawning was rejected as blurred ownership — who stops it, which instrument
loads, who cleans up orphans — even behind an opt-in flag (two documented behaviors instead of one;
the MVP persona is a dev with a checkout and a terminal). So a tool that needs the engine and finds
none **fails fast with an actionable error** — it names the fix (`start reuben play`) rather than
guessing. Since UDP is silent about a dead port, this needs a cheap liveness probe; the shipping code
either probes first (for the fire-and-forget OSC `send`, whose transport would swallow a dead port
silently) or acts-then-maps the unreachable error (for the structure-channel tools, one connection,
no time-of-check/time-of-use window).

**Multi-client is tolerated, not arbitrated.** Any number of shims may target one engine — two
conversations sharing one engine is a supported workflow from day one — and concurrent `send`s are
last-write-wins per control, exactly the semantics of two physical OSC controllers today, documented
rather than gated. Enforcing one-client-per-engine would mean lease/liveness machinery over a
connectionless transport, blocking the two-conversation workflow for no demonstrated need. Structure
edits, by contrast, are serialized by the single-writer Coordinator by construction (see the
execution-runtime topic), so nothing richer is required.

Distilled from: ADR-0044

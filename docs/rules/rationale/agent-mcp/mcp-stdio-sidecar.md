# Why: The MCP server is a disposable per-conversation stdio sidecar that hosts the pure tools in-process and forwards engine tools to a long-lived engine, with rmcp and tokio fenced in its own crate.

[Rule](../../agent-mcp.md#mcp-stdio-sidecar)

Conversational authoring needs an MCP server, and the server needs a home relative to the sound. The
fork was in-process (the MCP process hosts the `Engine`) vs a **sidecar** talking to a separately
running engine. Two facts decided it. First, MCP's stdio transport ties the server's lifetime to the
conversation — the client spawns it, and on conversation end closes stdin → SIGTERM. Whatever lives
in that process **dies with the chat.** Second, the pure introspection tools need no engine at all;
only the engine tools touch a live process. So: the shim hosts the pure tools **in-process** and
forwards the engine tools to a long-lived engine the user started, and the sound survives while
conversations stay cheap and disposable. The engine-in-process alternative was rejected because the
sound would die on stdin close and two conversations could never share an engine; Streamable HTTP was
rejected because it buys the churniest part of the spec (protocol sessions) for a persistence problem
the sidecar already solves, and no surveyed audio-MCP server chose it.

Two structural consequences hold today. The pure functions descend into `reuben_core::introspect`
(they already import only core types, so core gains zero MCP awareness) and the MCP adapter is a
**new bin crate `reuben-mcp`** — the first workspace member allowed an async runtime, with rmcp +
tokio (measured at ~34 lock packages, `transport-io` only, a `current_thread` runtime, no network
stack) **fenced there** so every play/CLI/web build stays std-only. rmcp earns its weight by
measurement, not by default: hand-rolling ~5 methods wins only for code discarded before the next
protocol break, and this shim is the epic's durable surface. The separate engine path this implies —
one loopback structure channel carrying both structure edits *and* control — is the seam the engine
tools drive ([tool-surface](tool-surface.md)). Control originally rode OSC/UDP instead, on the
grounds that a `send` is the same gesture a hardware knob makes; that split was reversed once it was
clear the sidecar and the engine are peers who already speak core's types, and that encoding OSC
between them bought a wire format the engine immediately decoded again. OSC-the-binary-protocol is `reuben play`'s
**foreign** edge — external controllers in, `osc_out` nodes out — not an internal hop.

Distilled from: ADR-0044

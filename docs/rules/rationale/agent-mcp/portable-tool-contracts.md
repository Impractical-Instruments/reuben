# Why: The tool contract types and introspection live OS-free in reuben-core, so every door — native CLI, MCP sidecar, web in-page layer, web proxy — generates its schemas from that one source and no verb means different things behind different doors.

[Rule](../../agent-mcp.md#portable-tool-contracts)

The durable artifact of the MCP effort is not the protocol — it is the **contracts**. MCP is one
thin door over them; the web in-page tool layer is another; the CLI is a third. What keeps them from
diverging is that the serde types the contracts are made of — the report/diag shapes, the diff
summary, the swap report, the content hash — and the introspection behind them live **OS-free in
`reuben-core`**, not in the native crate or the MCP crate. So the wasm lane reuses the exact types
the native lane serializes, and every door **generates its schema from that one source** rather than
hand-authoring a parallel copy that is free to drift. In the shipping code the roster itself is
single-sourced: `reuben_core::tools::CONTRACTS` declares the name-set and channel kind once, and each
door derives its advertised names from it — the descriptions and schemas stay per-door (they are
host-flavored and, for MCP, carry rmcp/schemars machinery core must never depend on), but the
identity is one authority.

This is why **web parity ports the contract, not the protocol.** No MCP reaches the browser: a tab
can only dial out, so the sidecar's dial-in shape cannot be copied, and every candidate desktop→tab
bridge answered a persona that does not exist. Instead the browser binds the same eight contracts
directly over the C-ABI worklet, same report shapes — MCP stays native-only, the cheapest layer in
the stack to swap because nothing beneath it is MCP-shaped. The one surviving line from the
(otherwise product-owned, now-private) web-chat host decision is exactly this invariant seen from a
fourth door: the web proxy declares its tool schemas **generated from `reuben-core`'s serde types**,
so the declared contract and the executed contract cannot drift. The hand-authored-schema
alternative was rejected everywhere it came up — a second copy free to drift silently is precisely
the divergence this rule exists to prevent. Because the schemas derive from types, contract drift is
a compile-time concern, not a documentation one.

Distilled from: ADR-0044, ADR-0048, ADR-0052, ADR-0054

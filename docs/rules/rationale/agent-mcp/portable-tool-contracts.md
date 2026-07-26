# Why: The tool contract types and introspection live OS-free in reuben-core, so every door — native CLI, MCP sidecar, web in-page layer, web proxy — generates its schemas from that one source and no verb means different things behind different doors.

[Rule](../../agent-mcp.md#portable-tool-contracts)

The durable artifact of the MCP effort is not the protocol — it is the **contracts**. MCP is one
thin door over them; the web in-page tool layer is another; the CLI is a third. What keeps them from
diverging is that the serde types the contracts are made of — the report/diag shapes, the diff
summary, the swap report, the content hash — and the introspection behind them live **OS-free behind
one window**, not in the native crate or the MCP crate. So the wasm lane reuses the exact types
the native lane serializes, and the **roster identity** is one authority: `reuben_api::tools::CONTRACTS`
declares the name-set, the channel kind and the one sentence each verb is advertised by, and each
door derives its advertised surface from it. Input schemas stay per-door — host-flavored, and for
MCP carrying rmcp/schemars machinery the portable crates must never depend on.

This is why **web parity ports the contract, not the protocol.** No MCP reaches the browser: a tab
can only dial out, so the sidecar's dial-in shape cannot be copied, and every candidate desktop→tab
bridge answered a persona that does not exist. Instead the browser binds the same contracts
directly over the C-ABI worklet, same report shapes — MCP stays native-only, the cheapest layer in
the stack to swap because nothing beneath it is MCP-shaped. The web proxy is the fourth door and
derives the same roster, so the contract it declares to a model and the contract the in-page layer
executes cannot name different things. A per-door duplicate of the *roster* was rejected everywhere
it came up — a second name-list free to drift silently is precisely the divergence this rule exists
to prevent.

Distilled from: ADR-0044, ADR-0048, ADR-0052, ADR-0054

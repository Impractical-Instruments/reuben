# Why: The tool contract types, the argument and result shapes they carry, and the introspection behind them live once in reuben-api, so every door generates its schemas from that one source and no verb means different things behind different doors.

[Rule](../../agent-mcp.md#portable-tool-contracts)

The durable artifact of the MCP effort is not the protocol — it is the **contracts**. MCP is one
thin door over them; the web in-page tool layer is another; the CLI is a third. What keeps them from
diverging is that the serde types the contracts are made of — the argument shapes, the report and
diagnostic shapes, the diff summary, the swap report, the content hash — and the introspection behind
them live **once, at the window**, not in the native crate or the MCP crate. So the wasm lane reuses
the exact types the native lane serializes, and the **roster identity** is one authority:
`reuben_api::tools::CONTRACTS` declares the name-set, the channel kind and the one sentence each verb
is advertised by, and each door derives its advertised surface from it.

The location matters and the intent survives the move. The engine was the original home because it
was the OS-free crate every door could reach; it is [no longer where the wire
lives](window-declares-its-own-types.md), so the one source is the window — the crate that owns the
serialization, the advertised prose and the schema derivation together. What a door still owns is its
*transport* and how it carries the sentence, not the sentence and not the shape.

This is why **web parity ports the contract, not the protocol.** No MCP reaches the browser: a tab
can only dial out, so the sidecar's dial-in shape cannot be copied, and every candidate desktop→tab
bridge answered a persona that does not exist. Instead the browser binds the same contracts
directly over the C-ABI worklet, same report shapes — MCP stays native-only, the cheapest layer in
the stack to swap because nothing beneath it is MCP-shaped. The web proxy is the fourth door and
derives the same roster, so the contract it declares to a model and the contract the in-page layer
executes cannot name different things. A per-door duplicate of the *roster* was rejected everywhere
it came up — a second name-list free to drift silently is precisely the divergence this rule exists
to prevent.

Deriving *from* the roster is only half of it: a door also has to **name** an entry, and a name
written as a bare string literal compiles against nothing, so a contract removed upstream reaches
that door as dead plumbing behind a verb it no longer advertises. The document verbs never had this
problem — a door names `reuben_api::authoring::<ArgsType>` per verb, so deleting the verb deletes
the type and the door stops compiling. The read and engine arms had no equivalent, because a
projection assembled as a JSON literal has no type to lose. So the roster emits its names as
symbols: one `tools::names::*` const per entry, from the same table that builds `CONTRACTS`, and a
door writes the symbol where it would write the literal. The signal a deleted contract owes its
consumers is then the same one a deleted argument type gives — a compile error at the site that
names it.

Emitting both from one table is the point, not the convenience. A hand-kept list of names beside
the roster would be the [parity marker](../code-as-grounding/parity-test-is-a-defect-marker.md)
this repo cashes rather than renews — a second name-list free to drift, which is the very thing the
paragraph above rejects. What the macro cannot derive is the *case*: `macro_rules!` cannot
lower-case an identifier, so the symbol and the spelling are two tokens of one entry, held to the
convention by the one test in `tools.rs` that can see both halves.

A door that cannot take the symbol keeps the literal and says why. rmcp's `#[tool(name = …)]` is
one: the attribute parses a string literal and a route key must exist before the router does. That
door already refuses to construct on a roster mismatch, in both directions, which is the stronger
guarantee the symbols only approximate.

Distilled from: ADR-0044, ADR-0048, ADR-0052, ADR-0054, ADR-0068, ADR-0071 · symbols decided in
issue #700 — settled directly, no ADR.

# Why: There is no instrument JSON Schema for agent grounding; an agent grounds on prose rules, operator ports, and the validator loop, and registry truth is guarded by same-commit native-versus-wasm describe parity.

[Rule](../../agent-mcp.md#grounding-not-schema)

The instrument JSON Schema — once auto-generated from the operator descriptors and served as a
resource — has **no grounding role in any lane.** Agents need prose rules + ports + a validator loop,
not a ~21k-token JSON Schema to eyeball; the `validate` tool already provides the mechanical check the
schema promised, against the loader that is the single authority
([loader-single-authority](loader-single-authority.md)). Its only other consumers needed not the
schema but *a registry truth independent of the artifact under test*, and both get a stronger source:
the web boundary's silent-drop tripwire becomes **same-commit parity** — native `describe --json`
output ≡ wasm introspection output, compared structurally at CI time, fresh-vs-fresh so nothing can go
stale — and the web live-eval reads fresh native describe output. With both consumers re-homed, the
schema is deleted outright: the committed file, the generator example, the staleness test, the MCP
drift test, the web artifact key, and the `reuben://schema/instrument` resource. The decision hardened
further — even the generator *source* was later retired as YAGNI, since carrying hundreds of lines of
dead-but-regenerable code plus its parity test wasn't worth it; the serde field types are now the sole
authority, and a future constrained-decoding experiment would re-derive `Descriptor → JSON-Schema`
from scratch.

Keeping the file as the pin's carrier was rejected: any committed witness recreates the
staleness-guard machinery for less value than a fresh comparison. The durable point is the negative
invariant future authors need to know — **do not reach for an instrument schema; it isn't there** —
and the positive replacement: grounding is prose + ports + validator, and registry equivalence across
doors is a same-commit parity check, an application of the same fresh-vs-fresh posture that keeps the
contract types honest ([portable-tool-contracts](portable-tool-contracts.md)).

Distilled from: ADR-0059

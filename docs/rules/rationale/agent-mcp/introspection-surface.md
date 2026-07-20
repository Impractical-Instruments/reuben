# Why: Introspection is thin pure functions over the static registry and the real load path — describe an operator, validate a document — with no query into a running engine.

[Rule](../../agent-mcp.md#introspection-surface)

The pieces an agent needs to author already exist as data — the registry iterates every operator's
descriptor in deterministic order (the same source the grounding is projected from) and the load
path already type-checks a document. The gap was never *capability*; it was **a closed feedback loop
the agent can drive**: inspect one operator without grepping Rust, and check a draft without
launching the audio player (which binds a device and makes sound). So the surface is two thin things
over static data and the real load path:

- **describe** — with no argument lists every registered operator; with one, dumps that operator's
  ports and params. It reads the live registry, so it can never drift from the operators actually
  compiled in.
- **validate** — runs the engine's own load + `Plan::instantiate` against a synthetic default audio
  config, catching structural, wiring, kind-mismatch, and cycle errors with **no device opened and
  nothing rendered**.

**Live-graph query is deliberately deferred** — inspecting a *running* rig's state. It has no
consumer (reuben is OSC-in only; there is no running-engine handle to interrogate) and would add a
runtime/OSC surface. The static-data + load-path pair is everything the authoring loop needs. The
logic is **pure library functions** returning serde-serializable reports, with the binary a thin
shell; tests exercise the real load/plan paths through the library, not a spawned process — and that
purity is exactly what later let the surface descend OS-free into core and serve every door
([portable-tool-contracts](portable-tool-contracts.md)). What validate cannot catch — a structurally
legal patch that makes no sound — is a skill's concern, not the validator's
([authoring-skills](authoring-skills.md)).

Distilled from: ADR-0020

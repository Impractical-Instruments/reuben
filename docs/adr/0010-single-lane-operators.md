# Single-Lane Operators; cross-cutting work lives above the operator layer

## Context

A synth may have many Voices, each spanning multiple Channels. The product is a set of **Lanes** (one Voice in one Channel). Either each operator author handles that Voice×Channel fan-out, or the engine does. Authoring ergonomics and AI-authorability (ADR-0004) are first-class, and operators should stay small and composable.

## Decision

- **Operators are single-Lane by default.** An author writes one Lane — a single Voice, single Channel, a block at a time: "given one input block and my state, produce one output block." The engine **fans the operator out across all Lanes**, owning the per-Lane state. The author/agent reasons about a single stream, never the matrix, and cannot botch fan-out. ("Single-Lane" is about stream multiplicity, not sample granularity — operators always process a whole block.)
- **Cross-cutting concerns are preferentially expressed *above* the operator layer**, as structural constructs rather than inside ordinary operators:
  - voicing / unison / note→Voice assignment → the **Voicer**;
  - mixing / summing → deterministic **fan-in connections** (fixed reduction order, ADR-0001), handled by the connection layer, not an operator;
  - panning / Channel spread / collapse → Channel-aware boundary constructs.
- **Operator-level access to the full Voice×Channel set remains an available escape hatch**, declared in the descriptor (which also tells the engine where fan-out expands/collapses, needed for buffer sizing). It is discouraged in favor of the structural options above.

## Consequences

- The engine carries fan-out machinery and per-(Voice, Channel) state arrays — real complexity, but centralized once instead of smeared across every operator.
- Mixing is a connection-layer concern, not an operator.
- All fan-out/fan-in shapes are known at Instantiate, so they cost nothing at Render.

## Update (ADR-0025)

The per-kind ordinal counting described here is no longer hand-written as a `pub const` block.
[ADR-0025](0025-single-source-operator-contract.md) computes it once inside the `operator_contract!`
macro, which emits the `IN_/OUT_/P_` index consts and the `Descriptor` from one declaration.

# AI-agent authorability as a first-class constraint

## Context

A stated goal of restarting reuben now is to lean on AI tools. The author wants agents to easily (1) understand the system code, (2) write new Operators, (3) patch Instruments, and (4) assemble Rigs into full playable systems. This is a cross-cutting constraint, not a feature — it shapes several architectural choices.

## Decision

Treat AI-agent authorability as a first-class design constraint, on the same tier as the "good button" principle. The mechanisms that serve it, decided across ADR-0001..0003 and here:

- **Self-describing Operators.** Each operator type carries an introspectable descriptor (ports + rich param metadata), separate from its process function. Agents reason over descriptors instead of reading implementation.
- **One recursive model.** Operator / Instrument / Rig are the same graph concept at every scale (ADR-0003). An agent learns the model once and applies it everywhere.
- **JSON canonical document format, with a JSON Schema auto-generated from the operator descriptors.** The schema is one source of truth shared by file validation, editor autocomplete, and agent grounding. JSON is chosen because models write it natively and tool-calling is JSON; comments/annotations are allowed so agents and humans can leave notes.
- **Referenced library.** Documents reference reusable Instruments by ID/path, so an agent composes by reference (pull in an existing chord-voicer) rather than regenerating it. Flatten-to-inlined exists for sharing.

## Considered and rejected

- **Human-first text format (RON or a custom patching DSL):** nicer for hand-editing, but models are weaker on them. Rejected — optimize the *text* format for the agent; human authoring is carried by the GUI.

## Consequences

- Descriptors and the generated JSON Schema must stay in sync — acceptable since the schema is generated from descriptors.
- An introspection/query API (list operators, inspect descriptors, traverse a graph) is likely needed so agents can explore a live system, not just files.
- A **suite of agent skills** is a product deliverable, not just an internal aid — serving three audiences: developers (scaffold a new Operator: Rust + descriptor + tests), patchers/power users (build and modify Instruments and Rigs via the JSON schema), and non-technical end users (natural language → Toy/Instrument/Rig). Tracked in ROADMAP.

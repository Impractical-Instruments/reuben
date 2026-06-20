# Recursive (fractal) composition

## Context

reuben has three scales of structure: Operators, Instruments, and Rigs. They could be three distinct types with separate code and rules, or one recursive concept at every scale. AI-agent authorability is a first-class requirement, and we already flatten the graph into a single topological schedule at plan-build time (ADR-0001).

## Decision

Composition is **recursive**. There is one underlying concept — a graph of nodes with typed Signal/Message ports — at every scale.

- An **Instrument** is a named subgraph that exposes boundary ports, and can therefore be used inside another Instrument or a Rig *as if it were an Operator*.
- A **Rig** is simply the outermost graph.
- Nesting is an authoring/organization concept only. At plan-build time, nested graphs are **inlined** into the single flat topological schedule, so recursion has **zero runtime cost**.
- Each reuse of an Instrument gets its **own operator identities and its own state** — no accidental sharing between instances. Boundary ports map inner-operator ports to the instrument's exposed ports.

## Considered and rejected

- **Three distinct layered types:** hard boundaries between Operator / Instrument / Rig. Rejected — it triples the implementation (three graph models, three schemas), caps composition at fixed layers, and forces an AI agent to learn three different mental models and file formats instead of one.

## Consequences

- One graph engine, one port model, one connection rule, one file schema — the single biggest lever for AI authorability (learn the model once, apply it from operator to rig).
- Requires careful per-instance identity and state isolation on reuse, and a boundary-port mapping — both tractable with the stable-identity machinery from ADR-0001.
- Beginners are shielded from pathological nesting by the Toy layer and good defaults.

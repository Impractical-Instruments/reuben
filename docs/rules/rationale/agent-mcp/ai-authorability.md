# Why: AI-agent authorability is a first-class design constraint, served by self-describing operators, one recursive graph model, an agent-native JSON format, a referenced library, and a suite of authoring skills.

[Rule](../../agent-mcp.md#ai-authorability)

A stated reason to build reuben now is to lean on AI agents to understand the system, write
operators, patch instruments, and assemble rigs. That is a cross-cutting *constraint*, on the same
tier as the "good button" principle — it shapes architecture rather than sitting beside it, so it is
recorded as a rule, not a feature. Four mechanisms serve it, and each is load-bearing elsewhere:

- **Self-describing operators.** Each operator carries an introspectable descriptor (ports + rich
  param metadata) separate from its process function, so an agent reasons over the descriptor
  instead of reading DSP. This is what makes describe/validate ([introspection-surface](introspection-surface.md))
  possible and what the generated grounding is projected from.
- **One recursive model.** Operator / Instrument / Rig are the same graph concept at every scale, so
  an agent learns the model once and applies it everywhere (the composition/operator topic owns this
  now).
- **An agent-native canonical format.** The document is JSON — models write it natively and
  tool-calling is JSON — with comments allowed so agents and humans can annotate. The human-first
  alternatives (RON, a bespoke patching DSL) were rejected: optimize the *text* format for the
  agent and carry human authoring in the GUI. (The JSON *Schema* once generated from descriptors was
  later dropped as agent grounding — see [grounding-not-schema](grounding-not-schema.md) — but the
  agent-native JSON *document* stands.)
- **A referenced library.** Documents reference reusable instruments by id/path, so an agent
  composes by pulling in an existing part rather than regenerating it.

The consequence the rest of this topic discharges: descriptors imply an introspection surface so
agents explore a live system, not just files, and the **suite of authoring skills** is a product
deliverable serving three audiences (developers, patchers, end users), each closing its own loop
([authoring-skills](authoring-skills.md)).

Distilled from: ADR-0004

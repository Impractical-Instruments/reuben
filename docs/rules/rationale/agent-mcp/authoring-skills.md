# Why: Each authoring audience has a skill that closes its own introspect-or-scaffold, draft, validate-or-test, report loop, and carries the semantic judgement the validator cannot (validate-pass is not audible).

[Rule](../../agent-mcp.md#authoring-skills)

The authoring-skill suite is a product deliverable, not an internal aid
([ai-authorability](ai-authorability.md)), serving three audiences — patchers (build/modify
instruments and rigs), developers (author a new Rust operator), and end users (natural language →
Toy). What each audience actually needed was never more *capability* but **a closed feedback loop**:
a way to draft, check without ears, and iterate. So every skill has the same spine — introspect or
scaffold to learn the ground truth, draft, run the check loop until it passes, report — differing
only in what its check is:

- **patcher** loops `describe` → draft the JSON graph → `validate --json` until `ok`.
- **create-operator** loops scaffold → implement the DSP test-first → close a richer gate
  (`cargo test`, schema regen, clippy, `describe`, a throwaway `validate`), because no single
  command proves DSP correct.

The load-bearing discipline is that the mechanical checks stop short of *meaning*:
**validate-pass ≠ audible.** A structurally legal instrument with a disconnected oscillator
validates clean and makes no sound; a filter that compiles and registers may not filter. That gap is
owned by the skill, which carries *moderate* semantic guidance (canonical sub-graph recipes, a
Signal-vs-Message note, the realtime authoring contract for operators) while staying thin on the
syntax the loader already enforces. Each skill also keeps a scope boundary — patcher does topology +
params only, not new operators or control blocks — so the audiences compose rather than overlap.

Distilled from: ADR-0004, ADR-0020, ADR-0021

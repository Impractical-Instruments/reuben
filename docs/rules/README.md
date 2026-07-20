# reuben rules index

The now-state architecture, as rules. Read top-down and **stop at the shallowest level
that answers your question**:

    index (this file)  →  topic doc   →  a rule         →  its rationale
    summaries+glossary     now-story +    present-tense     condensed "why",
                           its rules      statement         read only when needed

- A **topic** is one area of the system: its "now" story plus the rules that hold there.
- A **rule** is a present-tense normative statement with a stable anchor and one rationale link.
- A **rationale** is the condensed "why", loaded only when needed. Provenance lives there.

Code points at topics, never at rules or ADRs: `// see rules: <topic>` (this repo),
`// see engine rules: <topic>` (web → engine). See [Conventions](#conventions).

## Topics

<!-- derived — collated from each topic's `> summary`; do not hand-edit out of sync. -->
- **[Execution & runtime](execution-runtime.md)** — How the unified block graph is scheduled, threaded, swapped, and rendered in real time — the Plan lifecycle, RT boundary, determinism, latch service, and the embed surface.

## Glossary

<!-- derived — collated from each topic's `## Terms`, linking the defining topic. -->
- **Block** — the fixed-size processing quantum; each block computes message- and signal-domain data in one dependency-ordered pass. · [execution-runtime](execution-runtime.md)
- **Coordinator** — the single non-RT writer of graph structure; owns the canonical graph and instrument library and performs every Swap. · [execution-runtime](execution-runtime.md)
- **Embed surface** — the portable rim of reuben-core (the `Engine` bridge) that each host shell wraps; the native I/O layer is the removable other side. · [execution-runtime](execution-runtime.md)
- **Engine** — the portable bridge in reuben-core (`queue_osc` → `fill` → `drain_outbound`) a host shell drives, and the whole vessel (Plan + Renderer + scratch) that a Swap crosses. · [execution-runtime](execution-runtime.md)
- **frame** — a sample offset within a block; the unit of sample-accurate Message timing. · [execution-runtime](execution-runtime.md)
- **Instantiate** — the off-thread construction of a Plan (topo sort, cluster, allocate the delta); the first half of every Swap, where all allocation lives. · [execution-runtime](execution-runtime.md)
- **latch** — the engine-held per-port zero-order-hold of an input's last Message, read by an operator as its constant current value. · [execution-runtime](execution-runtime.md)
- **Plan** — the runtime artifact: the immutable, already-allocated static parallel schedule (topo-ordered, clustered) that Render executes per block. · [execution-runtime](execution-runtime.md)
- **Render** — the hard-realtime, allocation-free per-block execution of the current Plan on the audio thread. · [execution-runtime](execution-runtime.md)
- **survivor** — an operator that persists across a Swap (matched on address + type + instantiate-time fingerprint) and keeps its state via box transplant. · [execution-runtime](execution-runtime.md)
- **Swap** — the single off-thread transition that installs a new Plan/Engine at a block boundary, migrating survivor state and reclaiming the old vessel. · [execution-runtime](execution-runtime.md)

## Conventions

**Layout**

```
docs/rules/README.md                     index: topic summaries + derived glossary
docs/rules/<topic>.md                    now-story + rules; each rule links its rationale
docs/rules/rationale/<topic>/<rule>.md   condensed why + "Distilled from: ADR-00xx"
docs/adr/                                live ADRs (iteration surface); see docs/adr/README.md
```

**Rule** — a present-tense normative statement, one sentence. Carries a stable kebab-case
slug (unique within its topic) as a raw-HTML `<a id>` anchor above the heading, so the
sentence can be reworded without breaking links. Exactly one rationale link.

**Rationale** — the condensed "why" that still applies; superseded/dead-end paths are dropped
(git keeps them). Ends with a `Distilled from: ADR-00xx[, ADR-00yy]` provenance line. One file
per rule at `rationale/<topic>/<rule>.md`.

**Code-comment reference** — topic-level only, never a rule slug or ADR number:
`// see rules: <topic>` in-repo, `// see engine rules: <topic>` cross-repo. Grammar:
`/\bsee (engine )?rules: ([a-z0-9-]+)/`; the slug must resolve to a topic doc.

**Progressive-disclosure ladder** — index → topic → rule → rationale. Stop at the shallowest
level that answers the question; open a rationale only when you need the why.

**Derived index** — the Topics list and Glossary above are collated from the topic docs; do not
hand-edit them. The `pre-commit` hook regenerates them (`check_rules_derive.py --write`) whenever a
commit touches `docs/rules/`, and CI runs `--check` as a backstop. Run `scripts/install-hooks.sh`
once per clone.

**ADR lifecycle** — see [docs/adr/README.md](../adr/README.md).

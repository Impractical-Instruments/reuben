# Why: Grounding splits by direction, not persona: input handling (reading intent as moves) is shared base sauce delivered to every lane, while output filtering (the sound-not-machine persona) is host-owned flavor.

[Rule](../../agent-mcp.md#cross-lane-grounding)

The verb layer is already unified — every lane descends to the same introspect + loader — but the
prose/policy layer was not, and un-unified lanes make the pipeline's token/latency measurements noisy
(a layer's win can't be trusted when each lane serves different sauce). The right cut is **not** a
dev-vs-musician persona split; it is the **direction of the language**:

- **Input handling** — interpreting musical/mood/abstract language as patching moves — is identical
  in every lane (a dev patching in the repo says "warmer" too). It is the base sauce all lanes
  consume: the word→move table ([intent-vocabulary](intent-vocabulary.md)) plus the edge conduct for
  imperfect mappings (ambiguous ask → pick the most-likely reading, act, offer alternatives;
  unsatisfiable → offer the nearest achievable move).
- **Output filtering** — what the person is *shown*: sound-not-machine subject, hidden diagnostics,
  the plain→theory register ratchet, tone — **is** the persona. It is zero at skills/MCP and maximal
  at web, so it stays host-side as a composable module, not lane sauce. A dev harness never sees it.

The consequence is a clean rule: **MCP adds nothing over skills** but delivery — its differences are
subtractions — and page furniture (Keep gestures, first-run shapes) belongs to hosts, not lanes.
Every lane host reduces to transport bindings + host furniture + the base sauce. Delivery then splits
by **push vs pull**: push is bundled into context and paid every session (web's only channel today),
pull is a pointer or resource, free until followed (skills, MCP). So the guide, vocabulary, and
library index are pulled by skills/MCP and bundled by web (the web cut drops checkout-only sections by
lane tags). A "structural" third register tier and an engine-side home for the persona were both
rejected — the persona is product voice for one host, not lane sauce, and modeling depth where the
real variable is direction forces one doc to carry contradictory absolutes. This extends the
contract-ports-not-protocol stance ([portable-tool-contracts](portable-tool-contracts.md)) from verbs
to grounding: authored once, delivered per lane.

Distilled from: ADR-0059, ADR-0052

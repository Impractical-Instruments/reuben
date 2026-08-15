# Domain docs

<!-- GENERATED from templates/domain.md + .ii/repo.toml by `ii-generate --write .` — edit the source, not this file. sha256=da2bdf5fef982da466189f0a25de3aa61f2de0aa8106f85134c73e1924b34919 -->

Where this repo's domain documentation lives, and what kind of record each part is. Facts only — this file routes; it does not advise.

## The doc system

This repo documents its now-state architecture as a **rules system** under `docs/rules/`: one index, a doc per topic, individual rules, and a condensed rationale behind each rule.

### Read top-down

Stop at the shallowest level that answers your question.

- **`docs/rules/README.md`** — the front door: a summary per topic and the derived glossary. Start here.
- **`docs/rules/<topic>.md`** — the "now" story plus the rules for the area you are about to work in.
- **`docs/rules/rationale/<topic>/<rule>.md`** — the *why* behind one rule: the concrete problem it solves and the alternatives ruled out. Optional, and absent where the rule's sentence carries its own reasoning. Open it only when the rule alone does not answer you.

### Topics

- **[Agent framework & MCP](../rules/agent-mcp.md)** — How AI agents author reuben — authorability as a first-class constraint, the introspect/validate loop, the authoring skills, and the MCP sidecar whose tool contracts are one OS-free source behind every door.
- **[Authoring surface & instrument library](../rules/authoring-library.md)** — How authoring surfaces and the instrument library sit on top of the graph — decoupled surface docs over interface pipes, Good Buttons, the sample/resource store, library resolution and format versioning, and the launch Toys.
- **[Code as a grounding surface](../rules/code-as-grounding.md)** — How this repo's own source text is governed as grounding an agent reads — comment discipline that points at rules instead of restating them, LSP-first navigation, and pre-scoped search.
- **[Composition & operator model](../rules/composition-operators.md)** — The one recursive graph — how operators declare and register their contract, how all data flows as one Message/Arg substrate in Value, Event, and Signal forms, and how instruments nest and expose interface pipes.
- **[Execution & runtime](../rules/execution-runtime.md)** — How the unified block graph is scheduled, threaded, swapped, and rendered in real time — the Plan lifecycle, RT boundary, determinism, latch service, and the embed surface.
- **[Host shell & native I/O](../rules/host-shell-io.md)** — What a host shell owes the engine at the edges it owns — devices and their foreign clocks, the resampling and drift compensation that reconcile them, the latency that buys, and the fixed, counted way every edge degrades.
- **[Signal, OSC, musical time & DSP](../rules/signal-time-dsp.md)** — How signal and musical meaning are carried, timed, and shaped — the OSC-only Message model, the Clock and musical time, symbolic pitch and Tuning, the tonal-context bus, and the envelope/curve/math DSP families.
- **[Web/product boundary & dev process](../rules/web-product-process.md)** — How this repo sits under the web/product boundary: the BSD SDK a private product consumes, the raw C-ABI browser contract and sample-trust obligation it owes, and the release, toolchain, and perf-benchmark process that governs it.

That list is read from `docs/rules/README.md` at generation time, not stored here. Regenerating this file re-reads it.

This repo is **single-context**: one set of domain docs covers it.

## What `docs/adr/` is

The ADR surface here is **transient**.

- **`durable`** means `docs/adr/` is this repo's decision record. An accepted ADR stays; a decision that changes is superseded by a new ADR rather than by editing the old one.
- **`transient`** means `docs/adr/` is a live iteration surface and **not** the complete record. It holds one file per decision that is still moving; once a decision settles it is distilled into a rule plus its rationale and the ADR is deleted. The settled design is in the rules; nothing points back at the deleted ADR, and git history keeps what it argued.

---
name: patcher
description: Build or modify a reuben Instrument or Rig — the playable JSON graph of Operators. Introspects the live operator set, drafts or edits the graph, and validates it against the real engine load path before declaring done. Use when the user says "build an instrument", "make a synth/pad/bass", "patch up a rig", "add a node", "wire X to Y", "change this instrument", or describes a sound to construct.
---

# patcher

A reuben Instrument is one recursive JSON graph: **Operators** (nodes), each with an **`inputs`**
map (literals + wire-refs) and an optional **`config`** block, with declared **outputs**
([the rules index glossary](../../../docs/rules/README.md);
[composition-operators](../../../docs/rules/composition-operators.md)). Operator / Instrument / Rig
are *scales* of the same graph, not different file types — this skill authors all three. It grounds
itself on the **live operator set** (`reuben describe`) and proves its work on the **real engine load
path** (`reuben validate`) before finishing.

It does **not** author new Operators (that is Rust — the `create-operator` skill), author
surface docs (`surfaces/*.json` presentation — the `control-surface` skill), or edit core
crates.

## The loop: introspect → draft → validate → sync index → report

Run all `reuben` commands from the repo root.

1. **Introspect the operators you need.** Never guess ports/inputs — ask the binary:
   - `cargo run -q -p reuben-native --bin reuben -- describe --json` — every operator.
   - `cargo run -q -p reuben-native --bin reuben -- describe <op> --json` — one operator's
     ports (`name`+`kind`, where `kind` is the glossary word: `value`=a held `f32` Value (a knob,
     a gate, a latched pitch), `signal`=a dense `f32_buffer` Signal (audio, a per-sample sweep),
     `enum`=a shared `vocab` enum, `message`=a `Note` stream, `harmony`=`Harmony`),
     settable inputs (`min`/`max`/`default`/`unit`/`curve`), enum inputs (`variants`+`default`),
     resource slots.
   - `cargo run -q -p reuben-native --bin reuben -- describe <patch.json> --json` — a nested
     instrument's **boundary**: its `interface` pipes as if they were operator
     ports, each with its **declared** `Arg` type, range, default, and unit.
     This is what a `subpatch` node referencing that file exposes — wire against these names,
     never the child's internals.

2. **Draft the graph.** **Creating from scratch? Start from a scaffold, never a blank file.**
   `cargo run -q -p reuben-native --bin reuben -- scaffold-instrument --name <name>` prints a
   guaranteed-valid minimal document (`{format_version, instrument, nodes:[]}`) — edit its `nodes`
   and `interface`, then validate. This sidesteps the first-creation stall where a from-nothing
   document omits the required top-level `instrument` field (#146). Then check `instruments/index.md`
   (the generated library index — one line per available instrument: role + face signature) for a
   close-enough instrument before drafting a chain from scratch, or draft against an existing
   `instruments/*.json` (e.g. `chord-player.json`) rather than a blank file. Reuse mechanics
   (referencing an index hit by id via `subpatch`), the format rules the loader enforces —
   node `inputs` (literals vs wire-refs) and `config`, the wire-form rules and their one
   implicit coercion, `Constant`s, `interface` pipes, `resources` — and the recipe-role are
   the **authoring guide's** content, not this skill's: read
   [docs/agents/authoring.md](../../../docs/agents/authoring.md) (served to MCP clients as
   `reuben://guide/authoring`) and draft against it so step 3 passes first try.

3. **Validate — loop until `ok`.**
   `cargo run -q -p reuben-native --bin reuben -- validate <path> --json`
   Returns `{ok, errors:[{node?,port?,message}], warnings:[...]}`. Fix each error (it names the
   offending node/input) and re-run until `ok:true`. This runs the real `load_instrument` +
   `Plan::instantiate`, so it catches unknown type/input, duplicate address, type mismatches,
   constants-in-inputs, **and cycles** — without playing audio.

4. **Sync the library index — whenever you added, removed, or renamed an instrument, or changed
   its top-level `doc` first sentence or its `interface` face.** `instruments/index.md` is a
   generated artifact, and the `library_index_is_in_sync` test fails CI when it
   drifts from a fresh generation — so a new instrument or an interface/doc edit that skips this
   step passes `validate` locally but reddens CI. Regenerate and stage it in the same commit:
   `cargo run -q -p reuben-native --example gen_library_index` (rewrites `instruments/index.md`
   from every instrument; never hand-edit it). Pure edits to a node's `inputs`/`config` that
   touch neither the role line nor the `interface` leave the index unchanged — running it is
   still safe (idempotent), and re-running to confirm a clean `git diff` is the reliable check.

## Run the tests

The introspection commands are backed by `reuben_core::introspect`, unit-tested there:
`cargo test -p reuben-core --lib introspect::tests` — covers validate (accept, unknown-type
localization, cycle, advisory warning) and describe (list-all, one-op fields, unknown-op
error, compact mode, library-index lines). `cargo test -p reuben-native --test cli` covers
the native CLI's own shipped multichannel-fixture integration coverage.

## Scope

| Thing | Action |
|---|---|
| Instrument/Rig graph — nodes, `inputs` (literals + wire-refs), `config`, `interface` pipes (including promoting a player-facing control to an input pipe), resources | **author / edit** (validate before done) |
| Surface docs (`surfaces/*.json` — presentation binding pipes to widgets) | **never** — that is the `control-surface` skill; it delegates graph edits back here |
| New Operator types (Rust) | **never** — that is the `create-operator` skill |
| Core crates (Rust) | **never edit** — grounding comes from `describe` and the authoring guide |

## Report

End with: which instrument file, what was built or changed (nodes added/rewired, params set),
the final `validate` result (`ok`, any warnings), whether the library index was regenerated
(step 4 — say so when a new/renamed instrument or an interface/doc change required it), and how
to play it (`reuben play <file>`, the OSC address to send notes to). If you built a Good Button
or promoted
a player-facing control to an interface pipe, suggest the `control-surface` skill to author its
surface doc (TouchOSC and any host-side renderer read from it).

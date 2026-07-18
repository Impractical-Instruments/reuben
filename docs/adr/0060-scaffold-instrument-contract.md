# ADR-0060: `scaffold-instrument` — a first-creation grounding contract

## Status

Accepted (2026-07-18). Implements
[#158](https://github.com/Impractical-Instruments/reuben/issues/158) (Wave 2 of epic
[#156](https://github.com/Impractical-Instruments/reuben/issues/156)), closing the first-creation
stall [#146](https://github.com/Impractical-Instruments/reuben/issues/146). **Rides on**
[ADR-0048](0048-mcp-tool-surface-and-contracts.md) (§1: the contract roster and the pure/engine
split — this adds one pure contract), [ADR-0044](0044-mcp-stdio-sidecar.md) §2 (engine-free tools
are always available), [ADR-0045](0045-whole-document-edit-contract.md) (whole-document edit
contract: the document travels by value, in and out), [ADR-0049](0049-no-resource-bytes-over-mcp.md)
(the conversational surface returns documents, not disk effects), and the single-source roster
mechanism landed by [#157](https://github.com/Impractical-Instruments/reuben/issues/157)
([`reuben_core::tools::CONTRACTS`]). **Reaffirms** ADR-0020's rule that the loader is the single
validation authority: the scaffold is proven valid by round-tripping through the same `validate`
path, not by asserting its shape in prose.

## Context

First-creation of an instrument via the live model stalls (#146). Authoring a top-level document
from scratch, the model omits the required top-level `instrument` name field, and `validate`
rejects it: `InstrumentDoc` is `#[serde(deny_unknown_fields)]` with `instrument: String` required
(`crates/reuben-core/src/format/mod.rs`). The model then burns its rounds re-guessing the required
shape. Reshaping an *existing* document does not have this problem — the required fields are already
present, so the model edits within a known-good frame.

The gap is grounding, not capability: the model can author nodes and wires, but it has nothing valid
to start from. The reshape-from-template path already works; first-creation is the one authoring
gesture without a template.

## Decision

Add a **`scaffold-instrument`** contract that returns a **guaranteed-valid minimal instrument
document** the author edits-then-swaps, turning first-creation into the reshape-from-template path.

### 1. A single-sourced core producer

`reuben_core::format::scaffold_instrument(name: Option<&str>) -> serde_json::Value` mints the
minimal document by **serializing an `InstrumentDoc`** — so the emitted field spelling can only ever
match the real serde contract (there is no parallel hand-written JSON literal to drift). It emits
exactly:

```json
{ "format_version": 3, "instrument": "<name>", "nodes": [] }
```

the current `FORMAT_VERSION`, the required `instrument` name (default `untitled`), and an empty
`nodes` list. Empty `nodes` is valid and needs no registry to build. Every other field is optional
and omitted. A unit test round-trips the output through the real `validate` path and asserts
`ok: true` — the loader, not this ADR, is the authority that the seed is valid.

### 2. A deliberate roster addition, exposed across every door

`scaffold_instrument` joins `reuben_core::tools::CONTRACTS` (ADR-0048 §1) as a **`Pure`** contract,
grouped with the other read-only producers (`describe_operators`, `describe_instrument`, `validate`)
— immediately after `validate`, before the engine contracts. This is the one intended roster change
(#157's single-source mechanism means every door derives its name-set and count from `CONTRACTS`);
the core roster unit test's hand-typed anchor and its count (`8 → 9`, four pure / five engine) move
with it. The per-door description and schema stay per-door (ADR-0052 §5); the roster single-sources
the name only.

The doors:

- **MCP** (`reuben-mcp`): a `scaffold_instrument` `#[tool]` returning the document by value under an
  object root (`{ document: … }`), with an advertised `output_schema`. It auto-joins the read-only
  test's Pure-derived set.
- **Native CLI** (`reuben scaffold-instrument [--name] [--json]`): prints the document to stdout
  (pretty by default, one compact line with `--json`), pipeable straight into `reuben validate`.

### 3. Read-only and by value

The contract is **read-only** and returns the document **by value** (ADR-0045/0049): it never
writes to disk. Persisting a scaffolded document is the native lane's own gesture (redirect stdout,
or the host app's save) — the conversational surface hands back a document, exactly as the
whole-document edit contract already does for every other transport.

## Consequences

- First-creation becomes reshape-from-template: the model starts from a valid frame and edits within
  it, instead of stalling on the required-fields shape (#146 closed).
- All lanes learn the same start move; the authoring guide and the patcher skill now name it as the
  first step of drafting from scratch.
- One more pure contract on every door, single-sourced — adding it was one `CONTRACTS` entry plus
  the roster anchor, not a parallel edit in each door (the #157 payoff).
- The scaffold is a fixed minimal document, not a smart generator: it does not consult the registry
  or pick starter operators. If a richer starting point is ever wanted, it is a separate decision;
  this contract's whole value is that its output is *guaranteed* valid with no moving parts.

# Why: The mechanical half of authoring is deterministic codegen behind a reuben verb — scaffold-operator, scaffold-instrument — that hands the author a guaranteed-valid or compiling starting frame, leaving only the creative half.

[Rule](../../agent-mcp.md#deterministic-scaffolds)

Authoring has two halves: a **mechanical, deterministic** half (boilerplate that must be exactly
right) and a **creative** half (the DSP, the sound). An LLM gets the mechanical half subtly wrong —
an unsorted insert, a missed registration, a malformed descriptor, a required top-level field
omitted — far more often than it gets the creative half wrong, and those slips cost rounds. The move
is to make the deterministic half *deterministic code*, not something the model regenerates each
time, behind a first-class `reuben` verb that a human authoring by hand benefits from too:

- **scaffold-operator** writes a new operator file and its registration from a contract spec, with
  the descriptor filled in and a `process` stub — and an intentionally **red placeholder test**, so
  the author starts Stage B (behavior, test-first) with "make this pass" as the obvious first step; a
  green-on-arrival stub would invite shipping a silent operator. The error-prone part is *editing
  Rust source*, which is far more robust as tested Rust than as a regex script outside the crate.
- **scaffold-instrument** mints a guaranteed-valid minimal document by **serializing an
  `InstrumentDoc`** (so the emitted field spelling can only match the real serde contract — there is
  no parallel hand-written JSON literal to drift). First-creation stalled because a fresh top-level
  document easily omits the required `instrument` name and validate then rejects it; a valid seed
  turns first-creation into the reshape-from-template path that already works.

Both are proven valid by round-tripping through the same `validate` path, not by asserting their
shape in prose — the loader is the authority ([loader-single-authority](loader-single-authority.md)).
Both refuse to clobber and reject a malformed spec before writing anything, so re-running is safe.
The whole value is guaranteed-valid output with no moving parts: neither is a smart generator (they
do not consult the registry to pick starter operators), and neither writes DSP or picks a sound —
that stays the creative half the skills own ([authoring-skills](authoring-skills.md)).

Distilled from: ADR-0021, ADR-0060

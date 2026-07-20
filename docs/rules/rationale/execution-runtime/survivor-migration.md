# Why: Operator state survives a Swap by pointer-transplanting boxes matched on fully-qualified address, operator type, and instantiate-time fingerprint.

[Rule](../../execution-runtime.md#survivor-migration)

An operator instance *is* its state — state and instance are one `Box<dyn Operator>`, with no
extraction surface on the trait. So migration is **box transplant**: off-thread the Coordinator
matches its manifest of the installed Plan against the new Plan and precomputes a migration table of
`(old index, new index)` pairs; at install the callback runs a bounded loop of `mem::swap` over the
matched boxes — pointer swaps, no allocation, no drops — and the displaced cold instances land in
the retiring Engine and free off-thread with it. A state-transfer API (`extract`/`inject` on every
operator) or serializing state through bytes were both rejected: a large hand-written surface across
~40 operators bought now for cross-config migration nobody has asked for.

The subtlety is the **survivor key**. Matching on address + type *literally* silently undoes edits:
a transplanted box carries everything baked in at Instantiate — its `config` constants (a voicer's
pool size), its resolved resources (a sample player's decoded audio), and hosted sub-plans — so
bumping `voices` 4→8 would keep the old 4-voice pool playing, and re-uploading a sample at the same
path would keep the stale audio sounding. That breaks "the document is durable truth." So a node is
a **survivor** iff it matches on **fully-qualified address + operator type + instantiate-time
identity fingerprint**, where the fingerprint covers the normalized `config` block plus the content
identity of everything resolved at Instantiate (resource bytes, hosted sub-documents, recursively).
A changed constant, resource, or hosted document = state reset, exactly like a type change — it *is*
a different instantiation. Everything else — rewired inputs, changed params, new neighbours — leaves
a survivor a survivor, because latches live in the Plan (the new Plan's values win), not in the box
([latch-service](latch-service.md)). This gives authors a rule of thumb: *changing what a node was
built from resets it.*

Two invariants keep the fingerprint honest. It covers **only** instantiate-time inputs, never a
node's runtime inputs/params — that asymmetry is the whole point of the split. And a node whose
fingerprint cannot be computed **never survives** (the conservative fallback: `None` means reset),
while two equally-broken resolutions must fingerprint *equal* so a swap that leaves a broken sample
broken does not spuriously reset the node. The hash is non-cryptographic (FNV) — it guards against
accidental transplant of a changed instantiation, not against an adversary — with domain tags and
length prefixes folded in so adjacent fields cannot alias. After a transplant the new Plan has reset
consumer latches, so each surviving op re-asserts its on-change held outputs (`on_transplant`) to
avoid stranding a downstream reader on a stale default. The primitive itself takes a bare
`&[(old, new)]` index-pair slice so it cannot import the Coordinator — one-way layering preserved.

Distilled from: ADR-0046

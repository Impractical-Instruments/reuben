# Why: The agent authors through a closed vocabulary of path-addressed, stateless, engine-free document verbs, each applying one surgical edit to the named source, re-validating the whole document through the loader, and writing only if it is valid.

[Rule](../../agent-mcp.md#document-verbs)

An incremental edit surface has to answer two objections, and both are answered by construction:

- **No second authority.** A verb is a way to *produce the next document*, not a way to check one.
  Every verb reads the source, applies one surgical edit, and re-validates the **whole** document
  through the engine's own load-plus-instantiate path — [loader-single-authority](loader-single-authority.md)
  is untouched.
- **The vocabulary is derived, not invented.** The document format is already the spec; the verbs
  are cut from it. That was a mechanical fact for a while — a hand-written table of every format leaf
  against the verb that writes it, set-diffed in CI against the real format types — and it is a
  measured one now: the table was a completeness test of the window's own surface, maintained by
  hand, and the authoring eval already counts freehand JSON, which is exactly what an agent emits
  when no verb reaches what it wants.

What forced the change is cost, measured rather than assumed. The whole-document read is lossless
**by obligation** — a model on the hook to re-emit every byte it is not changing must hold every byte
it is not changing. On a 53-node instrument a one-value tweak cost ~2,098 re-emitted characters every
turn, and re-emission does collateral damage (a tweak that also drops the document's `doc` prose) that
no amount of care in the model reliably prevents. Factoring into subpatches — the old remedy for a
document that outgrew comfortable re-emission — shrinks the document but not the obligation.

**Write-iff-valid, and no transactions.** `LoadError` has no unwired-input or unreachable-node error,
so a lone unwired node loads clean and renders silence: `new → add → add → wire → wire` is valid at
every intermediate step and the build need not be atomic. This binds the graph rules, not just the
verbs — a generator→output reachability check must stay a *warning*, because as an error every
intermediate build step would fail the write and incremental authoring would become inexpressible.

**Removal is the one edit that can invalidate**, so it is the one that cascades. Deleting an address
the rest of the document still names leaves dangling wires (`UnknownNode`, fatal), so remove
auto-unwires every consumer and reports exactly what it broke — matching how a dissolved subpatch
already drops touching wires and announces a dark-degrade warning. Refusing instead would turn the
commonest structural edit into a multi-call discovery exercise. Rename rewrites those references
rather than dropping them.

The verbs are **stateless and path-addressed** because there is nowhere to hold a workspace: a
handle-addressed `docID` workspace needs a session, core is stateless, and the CLI is a cold process
per invocation. A value-addressed `(document, …) -> {document, …}` form was rejected outright — the
document rides the context *both ways*, scoring worse than a plain re-emit. And there is deliberately
no `replace_document(source, json)` escape hatch: every vocabulary gap would quietly route through it
and the API would never get finished. Closing that hatch is what makes a missing verb visible at all
— a gap has nowhere to hide but the eval's freehand-JSON count.

A verb's `source` is **opaque and door-resolved** ([portable-tool-contracts](portable-tool-contracts.md)):
the resolver's read half already loaded a nested voice patch door-abstractly, and the write half joins
it, so the native resolver writes a file and the browser's memory resolver is designed to write the
host store — not yet built, so the web door still takes documents by value. Two consequences follow
for the native lane and are accepted: the resolver
stops being read-only, and the MCP sidecar formally becomes a process that writes to disk. `expect`
stays optional per [expect-guard-is-a-door-concern](expect-guard-is-a-door-concern.md) — mandatory
would force a read before every write and double the call count — and the clobber window in fact
*shrinks*: whole-document edit held the file across an entire turn; per-call read-modify-write holds
it for one call.

Distilled from: ADR-0045, ADR-0066

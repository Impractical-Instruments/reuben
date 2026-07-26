# ADR-0071 — The window owns both ends of the structure channel, and the host supplies what only a host can

## Context

[ADR-0067](0067-core-splits-and-reuben-api-is-the-one-window.md) makes `reuben-api` the one window,
[ADR-0068](0068-reuben-api-declares-its-own-types.md) decides what flows through it,
[ADR-0069](0069-reuben-api-is-one-crate-with-two-feature-halves.md) decides what the crate is, and
[ADR-0070](0070-the-window-owns-advertised-prose-and-the-door-owns-its-store.md) draws the line a
second door found. All four were argued about *authoring* — reading and editing a document, which
happens with no engine anywhere.

The engine half is a different shape and asks a question the authoring half never did. Its verbs do
not run in the caller's process: a request crosses a channel, and there is a **server on the other
side of it**. `reuben play` served that side; the sidecar dialled it. Both are doors, so the wire
between them belonged to neither, and it was parked in `reuben-core::coordinator` — a crate that
serializes none of it.

## Decision

**The structure channel's envelope is the window's, and so are both ends of it.** The verbs
(`ping`, `swap`, `get_document`, `get_diagnostics`, `send`), their payloads, the NDJSON framing, the
default loopback address, and the batch bound all move out of the engine and into
`reuben_api::engine`. Nothing in the engine referenced any of them; they were there because there
was nowhere else that both ends could see.

Owning both ends is the point rather than a side effect. A wire has two implementations by
construction, and until now the answer to "what does a swap *mean*?" was written on the serving side
only — in the native crate, where a second host could not reach it. Moving it makes the meaning
single-sourced the way the document verbs already are: the `expect` compare, the retain-prior
report, the empty/over-long batch rejections, the "queued, not applied" ack.

**What a host still supplies is what only a host can know**, declared as one seam
(`engine::EngineHost`) beside the resource seam: what a path resolves to, where a control batch
goes, what the counters read, how the device output map is republished after a swap, and how long
the deferred free waits for the retired Engine to come home — a gate the host returns, not the
Coordinator handed back to it, so the reclaim protocol is not something a second host re-derives.
The native crate keeps its listener, its threads, its cpal device map
and its render-callback liveness gate — all of which are host-shell concerns and none of which the
window could hold without becoming a host itself.

**The advertised roster moves with the verbs.** `tools::CONTRACTS` was in the engine, which
advertises nothing; it now sits in the window and carries the sentence each verb is advertised by
as a field, so a contract without a sentence does not compile. That closes ADR-0068's "the one source becomes `reuben-api`" for the last surface still
outside it, and it is what lets `reuben-mcp` drop `reuben-core` from its manifest entirely — the
checkbox phase 1 could not tick, because the engine half was still reaching past the window.

**The status shape keeps speaking in the sidecar's voice, deliberately.** `EngineStatus` still has a
`sidecar` field whose docs say "the reuben-mcp crate version", because the advertised surface is
what a model reads and relocating a type is not a licence to reword it. The door passes its own
version in — the one identity the window cannot know — and renaming the field is a prose change to
make on its own merits, with the eval gate watching, rather than as a side effect of a move.

## Consequences

**`agent-mcp.md#expect-guard-is-a-door-concern` is overturned as written.** Its finding stands and is
the reason this is worth recording: the guard is not the engine's, because the engine's swap is
unguarded last-write-wins and one door has no Coordinator at all. But "belongs to each door" was
the wrong resolution of that finding. It belongs to the *window*, which serves every door that has
a Coordinator to compare against — and the web lane, which has none, still writes its own. One
implementation fewer, and the reason the third one survives is now stated rather than discovered.

**`agent-mcp`'s loopback-only rule is untouched, and it is worth saying why.** Loopback-only still
holds, and so does its second clause — one shared default address keeps server and client from
drifting. Only the address of the address changed: not a constant two doors both reach into the
engine for, but one the window owns and both doors are handed. A relocation that leaves a rule true
is not an overturn.

**The advertised surface did not move: 27 tools, 72,976 bytes, before and after.** One byte-neutral
improvement fell out — the `swap` tool used to advertise a second, differently-wrapped copy of the
`Diag` description that the other 22 tools carried, because it was the engine's type and they were
the window's. There is one now.

**No crate enables the engine's optional `schemars` feature any more, and CI already anticipated
that.** The last dependent was the sidecar, deriving the engine tools' output schemas from engine
types; it derives them from the window's now. The feature is not dead, because CI names
`reuben-core/schemars` explicitly rather than inheriting it — a comment on that step says, in as many
words, that the projection's completeness guard lives behind it and must not stop running if a
dependent drops the feature. A dependent just dropped it, and the guard still runs.

That sharpens a phase-5 item rather than completing one: "core sheds its optional `schemars`
feature" is safe for the contract types, whose derives now have no consumer at all, and is *not*
safe as stated for the format types, where removing the feature would delete a live guard along
with it.

**A host serving the channel now writes an `EngineHost` rather than a dispatch loop.** That is more
type to satisfy up front and materially less to get right: the previous shape let a host reimplement
the guard, the bounds and the report and stay green.

## Related

ADR-0067, ADR-0068, ADR-0069, ADR-0070. Issue #650 phase 3. Context topics: `agent-mcp`,
`execution-runtime`.

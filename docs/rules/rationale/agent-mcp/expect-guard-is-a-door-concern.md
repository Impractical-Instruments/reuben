# Why: The optimistic-concurrency expect guard belongs to each door, not to core.

[Rule](../../agent-mcp.md#expect-guard-is-a-door-concern)

Multiple clients are tolerated rather than arbitrated ([user-owned-engine](user-owned-engine.md)),
so a door whose clients can race needs a cheap way to say "install this only if the engine is still
playing what I last read". That is the `expect` guard: the client passes the `content_hash` it
believes is live, and a mismatch rejects the swap naming the hash that *actually* keeps playing.
No sessions, no leases — one string compare, under the lock the door already holds.

**Why the door and not core.** The guard has no logic to centralize. Its entire body is
`expected != installed_hash()` over an accessor core already exposes publicly; there is no
normalization, no version tolerance, no lease. What varies between doors is not the decision but
its *shape* — the structure channel answers with a distinct `Conflict` reply carrying both hashes
as structured fields, which is a wire choice that channel makes
([portable-tool-contracts](portable-tool-contracts.md) governs the types, not the arbitration).

**And core cannot serve every door anyway.** The doors that need a guard do not all reach the
engine the same way: the native structure channel goes through `Coordinator::swap_document`, but
the web in-page layer runs a restart-swap with no Coordinator at all and implements its guard in
JavaScript. A guard parameter on the core swap would therefore be inherited by exactly one of the
doors that needs one — and that door is the *only* non-test caller of the core swap at all;
everything else that would carry the parameter is a swap test, passing it empty. Kept there, it is
production-dead code reachable only from its own test, and a second place for the meaning of
"matches" to drift.

So core's swap stays what it honestly is: a single-writer install with **last-write-wins**
arbitration. Concurrency is a property of having concurrent clients, and that is a door's
situation, not the engine's. An embedder that later wants a guarded swap writes the same two-line
compare against `installed_hash()` — under whatever lock it already holds, which is what makes the
compare-and-swap atomic in the first place.

Decided in: issue #554 — settled directly, no ADR.

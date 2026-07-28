# Why: The optimistic-concurrency expect guard is the window's, applied inside the Coordinator lock so the compare and the swap are one critical section — every door with a Coordinator behind it shares that one implementation, and only a lane with no Coordinator writes its own.

[Rule](../../agent-mcp.md#optimistic-concurrency-guard)

Multiple clients are tolerated rather than arbitrated ([user-owned-engine](user-owned-engine.md)), so
a door whose clients can race needs a cheap way to say "install this only if the engine is still
playing what I last read". That is `expect`: the client passes the content hash it believes is live,
and a mismatch rejects the swap naming the hash that *actually* keeps playing. No sessions, no leases
— one string compare, inside the Coordinator lock, so the compare and the swap are one critical
section rather than two operations a second client can slip between.

**Why not the engine.** The engine's swap stays what it honestly is: a single-writer install with
last-write-wins arbitration. The guard has no logic to centralize there — its whole body is a
comparison against an accessor the Coordinator already exposes — and concurrency is a property of
having concurrent clients, which is a door's situation rather than the engine's. A guard parameter on
the engine swap would be inherited by exactly one non-test caller, with every swap test passing it
empty.

**Why not each door either, which is the correction.** That finding is right and its old resolution
was wrong. A channel has two implementations by construction, and "belongs to each door" licenses
every one of them to re-derive the same compare and answer in its own shape. The window serves every
door that has a Coordinator to compare against, so there is one implementation and one meaning of
"matches". The distinct conflict answer carrying both hashes is part of it: a wire choice, made once,
by the side that owns the wire ([structure-channel-envelope](structure-channel-envelope.md)).

**One implementation survives outside, for a stated reason — and the reason has changed.** The web lane
writes its guard in JavaScript. It used to be a lane with no Coordinator at all, running a restart-swap;
that is no longer true, and the sentence that said it never would be was resting on a mechanism argument
narrower than the claim. It now builds each new Engine off the render thread and installs through a
mailbox, like every other door. What keeps its guard outside is narrower and structural: the door's
*authoring* instance is a separate wasm instance from the one holding the Coordinator, and it never
constructs, so there is no `installed_hash` there to ask. Hashing its own store instead would answer a
different question — an unshipped edit at that key is not what is playing — and turn every guarded
reshape into a spurious conflict. So the discriminator is not *has a Coordinator*, it is *can reach the
hash the engine has installed*, and saying so here is what keeps it from being rediscovered as drift.

Two guards now live at the window and they are not the same guard: the document verbs compare against
the hash of the *source* they are about to rewrite, the swap verb against the hash the engine has
*installed*. Same shape, different question.

Distilled from: ADR-0071

# Why: Render only reads an immutable Plan and never allocates, frees, or blocks; all allocation lives off-thread in Swap.

[Rule](../../execution-runtime.md#render-is-allocation-free)

This is the one invariant the whole runtime is arranged to protect: the audio callback may never
allocate, free, or block, or it risks an underrun. Everything else follows from pushing all
allocation into [Swap](plan-lifecycle.md)'s off-thread Instantiate — voice pools, edge buffers,
scratch, the emit pool, and the latch store are all sized and allocated before the Plan is
installed, so Render against an already-allocated, immutable Plan reduces to "read the Plan, drain
lock-free queues." That smallness is the point: Render correctness becomes auditable.

It is why the swap **vessel is the whole Engine** and not a bare Plan — if the callback had to
resize the Renderer's edge arena or the Engine's scratch when channel counts change, that resize
would be allocation on the audio thread ([engine-swap-unit](engine-swap-unit.md)). It is why the
per-block emit pool and per-node emit scratch are preallocated and *cleared*, not freed, each block
([operator-message-emission](operator-message-emission.md)). And it is why held context is a `Copy`
struct with its heavy data in an immutable off-RT registry — snapshotting a `Vec`/`Box` would clone
and allocate mid-render ([latch-service](latch-service.md)). The discipline is enforced in tests
(`tests/rt_safe.rs`): steady state must stay allocation-free, including across message flow and
context changes. Preallocated pools are sized to absorb a typical block (e.g. the emit pool) and
only *cleared*, not freed, per block; growing past their cap allocates once, which steady-state
graphs never reach. Allocation that genuinely belongs to Swap setup — the mailbox slot pair, the
migration table — is done off the audio thread at Instantiate time.

One honest current gap is recorded in the code rather than hidden: `render_block` is
allocation-free, but `Engine::fill`'s message handoff (a `pending` Vec) still churns the heap when
messages flow, so the audio callback is not yet *fully* allocation-free — a lock-free preallocated
handoff is tracked for later. (`Arg::Str` is `Arc<str>`-backed, so a render-thread clone is a
refcount bump, not an allocation; only the last drop frees, at accepted sites.)

Distilled from: ADR-0009, ADR-0012, ADR-0046

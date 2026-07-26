# Why: An outbound Message carries a block-absolute frame, stamped once by the render loop and forwarded verbatim by the drain.

[Rule](../../execution-runtime.md#outbound-frames-are-block-absolute)

An operator does not see the whole block. When a held input changes partway through, the render loop
splits the block into segments and calls `process` once per segment, so the operator's own notion of
"frame 0" is the start of its current segment, not the start of the block. That is the right frame
of reference *inside* `process` — it is what lets an operator write per-sample code without tracking
where in the block it currently is.

It is the wrong frame of reference for anything outside. A Message leaving the system has to be
timed against the block the host is about to play, because the host knows nothing about segments;
they are an internal scheduling detail that depends on when controls happened to change. So the
render loop stamps each emission by adding its segment's start, converting segment-relative to
**block-absolute** at the one point that knows the offset, and the outbound drain forwards that
frame **verbatim**.

Both halves of "verbatim" are failure modes that have to be excluded by name. *Losing* the offset —
forwarding the segment-relative frame — collapses every emission from a later segment onto frame 0,
which is the bug that hides best: with one segment per block, the common case, segment-relative and
block-absolute are identical, so the code looks correct until a control changes mid-block. *Re-*
stamping — adding an offset again at the drain — double-counts and pushes emissions past the end of
the block. Either one breaks sample-accurate outbound timing and, worse, breaks it only under
conditions a casual test does not reproduce.

A single-segment block — the common case — makes segment-relative and block-absolute identical, so
only a test that splits a block at a known frame can see the difference; that is what pins it.

Decided in: issue #639 — settled directly, no ADR.

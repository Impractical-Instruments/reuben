# Why: The window is one crate split by feature rather than into two crates — an off-thread authoring half that declares its own types and a render half that re-exports — both on by default, with CI building the render-only configuration so the fence is a fact rather than an intention.

[Rule](../../web-product-process.md#window-is-two-feature-halves)

The two halves have opposite cost profiles, and conflating them is how the window goes wrong: the
authoring and control half is called from anywhere off-thread with an irrelevant budget and every type
serialized to a consumer, while the render half is driven from the audio callback where a conversion
per block *is* the cost. That is different enough to keep separable — a host that only wants Render
should not compile the authoring surface — and not different enough to be two crates. They share the
window's identity, and a consumer that wants both would otherwise take two dependencies to get one
boundary.

**Both halves default on.** A bare dependency line gets the whole window. The one consumer that must
not compile the authoring surface is the browser worklet, which lives in another repository and writes
an explicit `default-features = false` line — a line it would write under any default we picked. Making
every in-tree consumer declare its half to spare the out-of-tree one a line it writes anyway is a cost
with no payer.

**Which is exactly why the fence needs verifying rather than intending.** With both halves on by
default, no in-tree build exercises the render-only surface, so the fence's first failure would land on
someone else at a submodule bump. CI builds the render-only configuration, and that build is what makes
the split a fact.

The [resource seam](../authoring-library/resource-seam-is-host-implemented.md) turned out to belong to
neither half — the load behind the render half's constructor calls the same seam a document verb calls
— so it sits above both and the authoring half re-exports it for the doors that only drive that one.
Its shareable filesystem implementation is a third feature, off by default, because it is an
implementation rather than the seam.

Distilled from: ADR-0069, ADR-0072

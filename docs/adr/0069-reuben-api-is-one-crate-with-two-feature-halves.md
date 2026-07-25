# ADR-0069 — reuben-api is one crate with two feature halves, and the resolver ships unprivileged

## Context

[ADR-0067](0067-core-splits-and-reuben-api-is-the-one-window.md) makes `reuben-api` the one window
every consumer goes through. [ADR-0068](0068-reuben-api-declares-its-own-types.md) decides what
flows through it. Neither says what the crate *is*, and ADR-0067's Open list names one piece of that
explicitly: feature-gating, so a host that only wants Render does not compile the authoring surface.

Building the crate forces the question, because a skeleton has to pick a shape before it has any
content to justify one. Two things are being decided here: the axis the crate splits on, and where
the filesystem resource resolver lives now that it is a shared thing rather than a native one.

## Decision

**One crate, split by feature, not by crate.** `reuben-api` has two halves with opposite cost
profiles — authoring/control is called from anywhere off-thread with an irrelevant budget and every
type serialized to a consumer; Render is called from the audio callback, where a conversion per
block *is* the cost. Those are different enough to keep separable and not different enough to be two
crates: they share the window's identity, and a consumer that wants both would otherwise take two
dependencies to get one boundary.

**[ADR-0068](0068-reuben-api-declares-its-own-types.md)'s "the API declares its own types" stops at
the render boundary.** That decision was argued about *wire* types — things serialized and
advertised to a model — and a render handle is not one. Declaring an API-owned `RenderSlot` would
buy no decoupling, because nothing about a render handle is a serialization concern, and it would
cost a conversion on every block. The perf gate would reject that on the merits, and it would be
right to. So the authoring half declares its own types and the render half **re-exports**, with an
`#[inline]` passthrough as the most it may cost.

The limit is written down here rather than left to be inferred, because the failure mode is
somebody applying the earlier decision uniformly in good faith and discovering the cost as an
instruction-count regression with no record of why the boundary was ever drawn.

**`default = ["authoring", "render"]`.** A bare dependency line gets the whole window. The one
consumer that must not compile the authoring surface is the browser worklet, which lives in another
repository and takes `default-features = false, features = ["render"]` — an explicit line it would
write under any default we picked. Making every in-tree consumer declare its half to spare the
out-of-tree one a line it writes anyway is a cost with no payer.

**The fence is verified, not intended.** With both halves on by default, no in-tree build exercises
the render-only surface, so the fence's first failure would land on someone else at a submodule
bump. CI runs `cargo check -p reuben-api --no-default-features --features render`, which is what
makes the split a fact.

**The filesystem resolver is host-implemented, and we ship a reference implementation behind a
default-off `fs-resolver` feature.** The trait is the contract; the implementation is not. Off by
default is the whole point: a resolver that arrives switched on is one a host inherits rather than
chooses, and the resource seam is the one call *in* — the place a host's authority over its own
sources has to be explicit. A host whose sources are not files writes its own. It is not a separate
crate, because a crate for one struct is overhead with no boundary behind it.

**The reference resolver keeps implementing core's resolver trait for now.** It moved out of
`reuben-native` unchanged. What the window's *own* resolver trait looks like — its methods, whether
canonicalization and the write half survive as-is at the boundary — is a decision that belongs with
the authoring surface it serves, not with the skeleton that houses it.

## Consequences

**ADR-0067's feature-gating open question is closed.** The rest of its Open list — how many crates
the core splits into, whether `reuben-core` survives as a name, sequencing — is untouched and still
open. This decides the window's shape, not the split behind it.

**Two consumers now reach the resolver through the window.** `reuben-native` and `reuben-mcp` both
take `reuben-api` with `fs-resolver`, and `reuben-native`'s `resources` module is gone rather than
kept as a forwarding shim — a re-export whose only job is to postpone ten lines of churn is a thing
that stops getting deleted.

**`reuben-mcp` no longer depends on `reuben-native`, and `cpal` leaves its tree with it.** The
resolver was the last thing that edge carried. The agent-surface eval job stops installing ALSA dev
headers, which it only ever needed transitively; the workspace test job still needs them, because it
builds `reuben-native` for real.

**The crate is a skeleton and reads like one.** `authoring` and `render` gate documented but empty
modules. That is honest about what has landed, and it means the feature axis is exercised by the
build from the first commit rather than retrofitted once there is something to gate.

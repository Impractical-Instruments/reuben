# Why: An instrument's reuse story is the first sentence of its doc field, trusted for selection only, and discovery is a generated signature-line index over the available-set rather than a hand-kept curated list.

[Rule](../../authoring-library.md#recipe-role-and-index)

Reuse is worthless if a consumer (a human, or the web-chat patching agent) cannot find the right
instrument to reuse. Two things carry that: what an instrument is *for*, and how the set of them is
discovered. Both are answered by **generation from the document, never a hand-kept digest**.

An instrument's reuse story — its **recipe-role** — is the first sentence of its top-level `doc`
field, in the domain language, stating what it is and when to reach for it. Authored once at
creation, and kept true by the edit contract rather than by tooling: every whole-document reshape
re-emits the `doc` line, so the role is mechanically re-presented for revision on every edit (an
incremental edit layer would have made role drift structural — this synergy argued *for* whole-doc
edits). The crucial guard is the **trust boundary**: the role line is trusted **for selection only**.
The face — pipe names, `Arg` types, defaults, outputs — is always projected mechanically from the
`interface` block and enforced by the loader; no consumer takes face facts from prose. A wrong role
line can cost a bad-sounding attempt (wrong child chosen); it can never mis-wire a document.

Discovery is a **generated signature-line index** over the **available-set** (every instrument a
session can reference): one line per instrument — name + recipe-role line + interface face —
projected mechanically through the real load path, so the index never vouches for an instrument the
engine would refuse. It is the same generated-view family as the compact operator description,
staleness-tested like every generated artifact. There is **no curated list**: curation dissolves
into *quality* (authoring — give the instrument a face and a role line worth reusing) and
*availability* (delivery — which documents a session can reference, a separate concern). A hand-kept
curated list was rejected as a drift pair with the documents it describes plus an admission process
nobody owns; shipping full documents in grounding was rejected because the ~30–60-token index line
grounds everything selection needs against ~500–2,000 tokens of body that grounds nothing the face
does not — the full document stays fetchable on demand as the fallback when a role seems off.

Distilled from: ADR-0057

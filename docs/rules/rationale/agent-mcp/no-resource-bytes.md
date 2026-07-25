# Why: No reuben-owned bytes ride the agent's context: using a sample is a filesystem gesture the agent performs with its own file tools, a document is moved by the door's resolver behind an opaque source, and in the browser bytes reach the engine only through the staging seam.

[Rule](../../agent-mcp.md#no-resource-bytes)

"Use this sample" is the one authoring gesture that could have justified a byte-accepting tool, and
it does not. The transport fact frames everything: MCP's only client→server byte path is **base64 in
a tool argument, flowing through the model's context window.** A five-second stereo WAV is ~1.3 MB of
base64; a real sample library is hopeless. So there is no upload tool. Instead the agent writes
(copies, moves, synthesizes) the file **next to the instrument** with its own file tools, adds a
`resources` entry, and references it by relative path — sibling-first resolution makes that the
blessed location. `swap` is path-only for the same reason: durable truth lives at a source, not in a
payload. The loop self-corrects without new
machinery — validate stats the file and reports a missing one as a node-localized warning; an
undecodable file surfaces in the swap report as a dark-degrade warning — announced, not discovered by
ear. An `upload_sample` tool taking base64 was rejected as a file-write tool in a costume:
reintroducing the write path just ruled out, over a transport useless beyond toy clips.

This is **not scheduled for a later milestone.** What would revive byte-upload is not time passing
but the *persona* changing — a packaged, non-dev client without file tools — and packaging for
non-devs is out of scope. If that line is ever redrawn, byte-upload returns as part of *that* effort,
with its real transport questions answered in context. In the browser, where the engine has no
filesystem, the question was never "does MCP accept bytes" but **resource delivery**: bytes reach the
memory-resolver-backed engine only through the existing **staging seam** (stage-on-miss), fed by page
gestures (fetch from the asset base, a file picker later). The same posture holds there — no resource
bytes ride the agent's context on any lane; the agent references by key, and the host moves the
bytes.

The principle later **widened to cover the instrument document itself**, which it had never addressed
only because documents were assumed to be the model's own output. They are not: a document is now
named by an opaque source, read back as a projection, and written by the door's resolver
([document-verbs](document-verbs.md), [document-projection](document-projection.md)). That was already
half-true — voices and subpatches are documents referenced as resources — and it retires one clause
here: the sidecar's old "a filesystem write is the *agent's* own gesture" stance holds for samples and
not for documents, where the write is the door's. The **sample gesture is unchanged**, and the stated
revival condition for byte-upload is untouched: what widened the rule was a document joining the
sample as "a resource the agent references, never carries", not the persona becoming a packaged
non-dev client.

Distilled from: ADR-0049, ADR-0066

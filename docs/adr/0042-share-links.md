# ADR-0042: Share links carry a self-contained bundle + a live-state sidecar in a self-versioned envelope

## Status

Accepted (2026-07-09). The share-link decision of the web player epic
([#151](https://github.com/Impractical-Instruments/reuben/issues/151), P6:
[#228](https://github.com/Impractical-Instruments/reuben/issues/228)) — settled while building
P6, which puts a play-link on every sample-free Toy in the README rig table. **Rides on**
[ADR-0041](0041-web-player-app-in-repo.md) (the in-repo `/web` app this link boots),
[ADR-0040](0040-raw-c-abi-worklet-boundary.md) (the `reuben-web` C-ABI shell — P6 adds a
`format_version()` export to it), [ADR-0038](0038-interface-pipes-and-the-device-layer.md)
(the wired-slot union a knob value would overwrite), and
[ADR-0036](0036-instrument-library-and-format-versioning.md) §1 (the document is the save
source of truth) and §4 (additive changes never bump `format_version`; the format is
fail-closed). This record captures the four decisions the code deliberately does *not*
explain — each is a thing a later reader would otherwise "simplify" back.

## Context

The feature boots the player from a URL-encoded instrument in `location.hash` — paste a link,
hear music, no server round-trip and no install. The obvious implementation ("stuff the
document in the hash") is wrong in four independent ways, and each way is invisible in the
code that gets it right:

- An instrument document is **not self-contained**. `groovebox` (`instruments/groovebox.json`)
  names three voice patches through its `resources` table, which the browser resolves by
  fetching `instruments/voices/*.json` from the origin (ADR-0036 §3, the fetch-on-miss
  resolver). A bare-document link inherits that origin dependency.
- Turning a knob **never writes to the document** — it sends a control message to the engine
  (ADR-0038's pipes; the auto-UI's `control` blocks feed them). A document-only link is
  byte-identical no matter what you played.
- The player decodes a WAV sample through a **hand-rolled binary parser** (`hound`, via
  `decode_wav_bytes` in `crates/reuben-web/src/decode.rs`). Today every WAV byte comes from
  our own build; a share link would let a stranger's bytes reach it.
- `format_version` announces a *document-shape* change (ADR-0036 §4). It does not, and cannot,
  announce that the *link's own byte layout* changed. Those are different failures.

## Decision

### 1. The link carries a BUNDLE, not a document

The envelope carries the document **and** its transitive resources (the voice patches, and
later samples), captured once at share time from the sharer's origin. The link is therefore
**origin-independent**: it plays byte-for-byte the same on any host, or on none. The corollary
is the load-bearing part — a bundle miss is a **hard failure, never an origin fetch**. The
decoder is handed a `fetchResource` that reads from the bundle and throws on a miss; it never
falls back to `fetch()`. A link that resolved its own resources from the current origin would
boot differently depending on who served it and 404 differently depending on who didn't —
exactly the coupling the bundle exists to sever. "Resolve from the bundle only" is not an
optimization; it is what makes a link a link.

### 2. Live control state travels as a SIDECAR, not folded into the document

The document round-trips **byte-identical**; a snapshot of the controls the player changed
travels *alongside* it in the envelope. This preserves ADR-0036 §1 (the document is the save
source of truth) across a wire: the thing you shared is still the patch, plus a separate record
of how it was being played. Folding knob values back into the document is deferred to an
explicit **Export**, where a conflict can be refused *visibly* — because the merge is not safe
to do silently. `inputs[name]` is a union — `InputValue::Wire { from } | Symbol | Number`
(`format.rs`), **one slot**. Writing a knob value into a slot that currently holds a wire
deletes the wire and yields a schema-valid document that is *not the patch*. A sidecar keeps
the wire intact and keeps the destructive write on a path (Export) that can see the collision
and say so. Live re-encoding (updating the hash *as you play*) is a later rung of the epic
(P7, [#229](https://github.com/Impractical-Instruments/reuben/issues/229)); P6 ships the
snapshot, not the stream.

### 3. `kind = 1` (WAV samples) is refused as a TRUST BOUNDARY, not a size limit

Sample resources are excluded from the envelope. This is the decision most likely to be
reverted by someone reading the size table, seeing `sampler` (6.2 KB) fits comfortably under
the fragment cap, and "fixing" the exclusion. The reason is not size. Samples are the **only**
bytes in a link that reach `hound` via `decode_wav_bytes`, and that parser trusts the WAV
header's declared data-chunk length without checking it against the bytes actually present: a
44-byte WAV declaring a multi-gigabyte `data_len` provokes a multi-gigabyte allocation inside
wasm32, which aborts and traps (ADR-0040 §3 — a trap that may predate the panic hook). Text
resources instead go to `serde` with `deny_unknown_fields` (fail-closed, ADR-0036 §4). None of
this is reachable today, because every sample byte comes from our own build; a share link is
precisely the thing that would make `hound` reachable from a pasted text message. Excluding
`kind = 1` means **zero hostile bytes reach `hound`**. Sample-bearing links are a follow-up
that depends on hardening `decode_wav_bytes` first (bounds-checking the declared chunk length
against the buffer). Sample-free instruments — the whole README rig table today — lose nothing.

### 4. The envelope carries its OWN version, outside the compression

The link is `#r1.` (a literal prefix) + `base64url(deflate-raw(binary TLV))`. The **`r1`** is
the *envelope* version, and it is deliberately **outside the compression**, distinct from the
document's `format_version`. "The bytes are laid out differently" (envelope version) and "the
document shape changed" (`format_version`) are different failures with different remedies, and
the prefix must be readable **without decompressing** — otherwise "a link from a newer reuben",
"a truncated link", and "someone pasted `#about`" all collapse into one useless
`inflate: invalid` error. The C-ABI gains a `format_version()` export (ADR-0040) so JS can tell
an envelope-from-the-future apart from a document-from-the-future without guessing.

The corollary reaches back into ADR-0036. Because additive format changes never bump
`format_version` (§4) and the format is `deny_unknown_fields`, the realistic way a link from a
newer reuben fails is **an unknown operator** — it arrives as a perfectly valid
`format_version: 2` document that fails *structurally* at graph build, not at the version gate.
So ADR-0036 §4's "the engine and its instruments version together; upgrade the engine" stops
being a purely local invariant the moment a share link makes the *sender's* engine version a
property of a URL a stranger sent you. The failure taxonomy (A–I, all landing on the launcher
as a dismissible banner) must therefore be able to say **"upgrade reuben"** for a structurally
valid-but-unbuildable document, not "this instrument is broken."

### 5. The envelope's shape and its caps (supporting)

The TLV is bounds-checked on the way in (every length read is validated against bytes
remaining before it is trusted — the discipline `hound` lacks and §3 quarantines). The caps are
security guardrails, not ergonomics: **16 KB** encoded fragment, **1 MB** decompressed
(enforced *streaming*, so a zip-bomb is refused before it inflates), **64** resources, **256 KB**
per resource. QR codes are **explicitly out of scope**: byte-mode caps a QR near ~2953 bytes,
under `groovebox`'s 4.0 KB link, so a QR path would silently exclude the very rigs that most
want sharing — deferred rather than shipped half-working.

## Consequences

- **Boot-from-link replays the SENDER's version — accepted asymmetry.** Boot from a link, turn
  a knob, reload: you replay the *sender's* rendered state (the sidecar), not your edit, because
  live re-encoding is deferred (§2; P7, #229). This is honest for a snapshot and wrong for a
  live document; P6 ships the snapshot.
- **A moved schema default can play through an old link — bounded, rare, accepted.**
  `widget.default` is derived from the document **and** `schema.json`, and the schema comes from
  the *player's origin*, not the link. So an untouched control in an old link plays the *new*
  default if a schema range or default moves between share and open. It is bounded (only
  untouched controls, only on a schema change) and accepted; a control the sidecar captured is
  pinned and immune.
- **This is the try-before-install seam ([#221](https://github.com/Impractical-Instruments/reuben/issues/221)).**
  The registry's "every entry is playable in the browser before installing" rung is a
  self-contained bundle link by another name; §1 is the mechanism it will reuse, and §3's
  sample-hardening is its blocker for sample-bearing entries.
- **Input-taking instruments get a link but need a gesture.** `mic-space` is sample-free, so it
  earns a link; the mic affordance ([#248](https://github.com/Impractical-Instruments/reuben/issues/248))
  is what keeps that link from booting a silent page. The link carries the patch; the mic
  permission belongs to the instrument, not the envelope.
- **The README predicate is "sample-free *and* web-buildable", not just "sample-free".** The
  generator excludes `stereo-sub` even though it carries no samples: it declares three output
  channels and the worklet renders stereo (two), so a minted link could only ever fail to
  construct. A link that cannot boot is worse than no link, so the generator drops it (logged at
  generation) rather than committing a dead play-link. The refusal is web-engine-shaped, not
  envelope-shaped — the envelope would carry `stereo-sub` fine; the *player* can't render it.

## Alternatives considered

- **Bare document in the hash (resolve resources from the origin at boot).** Rejected: it makes
  a link boot differently per host and 404 off ours — the origin dependency §1 exists to sever.
- **Fold live state into the document (no sidecar).** Rejected: byte-identical no matter what
  you played, and the write-back that would fix that silently deletes wires (§2's union). The
  destructive merge belongs on Export, where it can be refused visibly.
- **Admit samples under a size cap.** Rejected: reads as ergonomics, is actually a trust
  boundary (§3). `sampler` fits the cap and still routes attacker-controlled bytes to an
  unhardened parser. Harden `decode_wav_bytes` first, then lift the exclusion.
- **One version number (reuse `format_version`, or version inside the compression).** Rejected:
  conflates two failures, and an inside-the-compression version can't be read until after the
  decompression that a from-the-future or truncated link makes fail (§4).
- **QR codes now.** Rejected: byte-mode caps below the common link size; a path that silently
  drops `groovebox` is worse than none.

# Why: Externally-sourced sample bytes are untrusted: the WAV decoder must bounds-check its declared data-chunk length before any sample-bearing share bundle can carry them.

[Rule](../../web-product-process.md#sample-bytes-trust-boundary)

The WAV decoder (`hound`, via `decode_wav`) trusts the WAV header's declared data-chunk length
without checking it against the bytes actually present: a 44-byte WAV declaring a multi-gigabyte
`data_len` provokes a multi-gigabyte allocation, which on `wasm32` aborts and traps (and a trap may
predate the panic hook, so it surfaces as an opaque `unreachable`). Today every sample byte the
engine decodes comes from our own build, so the parser is never reachable from hostile input — but a
**share link is precisely the thing that would make it reachable from a pasted text message**, routing
an attacker's declared length straight into that allocation. Text resources are safe by contrast:
they go to `serde` with `deny_unknown_fields`, fail-closed.

So the exclusion of sample resources from a shareable bundle is a **trust boundary, not a size
limit** — the decision most likely to be "fixed" by someone who sees a small sampler fit under the
fragment cap and lifts the exclusion. It reads as ergonomics; it is actually the only thing keeping
zero hostile bytes out of an unhardened parser. The share-link codec itself now lives in the product
repo, but the obligation it leaned on stays owed by **public core**: `decode_wav` must bounds-check
the declared chunk length against the buffer before any sample-bearing link can exist. Until then,
sample-free instruments lose nothing and sample-bearing ones simply have no link. The general
discipline — validate every declared length against bytes remaining before trusting it — is exactly
what the (now-private) envelope's TLV reader did and what `hound` lacks.

Distilled from: ADR-0042, ADR-0056

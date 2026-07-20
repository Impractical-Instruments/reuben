# Why: The conversational authoring loop is verified by a fixed menu — live-channel integration tests, Coordinator-direct behavioral swap checks, allocation-counting for RT-safety, and scripted human rituals where automation cannot reach.

[Rule](../../agent-mcp.md#conversational-loop-verification)

The conversational loop is this codebase's newest, least-precedented surface: a request/response
protocol channel, and a live-audio swap whose correctness is a *behavioral* property (does state
survive a rewire) rather than a structural one — operator state is opaque, with no introspection
trait to read it. So verification is a **fixed menu**, each item matched to what it can actually
prove, reusing conventions the repo already has:

- **The structure channel** gets a live-server integration test: stand up the real channel
  in-process, drive a raw client through every verb, and assert each response's shape
  **field-by-field**. This proves the actual framing and verb dispatch, not just that the serde types
  round-trip — a types-only test would pass a protocol bug like NDJSON line-splitting. The wire
  framing and `reply`-tag names are additionally pinned as literals at the unit level in
  `coordinator/wire.rs`, where a rename reds a fast test with no live server to stand up.
- **Swap correctness** is proven **off-device**, Coordinator-direct: bypass the channel, invoke the
  same install-check the audio callback calls, render blocks, and assert **behaviorally** (a
  rewired-neighbor envelope keeps decaying smoothly with no re-attack; a voice-count bump resets the
  pool). Behavioral because state is opaque; Coordinator-direct because the channel's own correctness
  is already the live-server test's job.
- **RT-safety of the install step** is an **allocation-counting** assertion (zero heap alloc across
  the callback-side install), reused from the existing RT-safety pattern — a binary correctness
  invariant, so allocation-counting proves it directly; no instruction-count bench, because the
  invariant is pass/fail, not a trend to watch.
- Everything automation **cannot reach** — device-level teardown/reopen, the audible swap gap, and
  perceptual judgment ("is it rounder?") — is a **scripted human ritual**: a checked-in starting
  document and exact prompt/commands, so the scenario reproduces run to run, with only the perceptual
  call left to a human ear.

The standing principle generalizes: where a verification can't be reliably automated, script the
human test (precise instructions + copy/paste commands) rather than leave an unscripted "go check it
manually." No new CI job or bench workload is added — the automated items ride the existing test
step.

Distilled from: ADR-0053

# ADR-0053: Verification strategy for the MCP conversational loop

## Status

Accepted (2026-07-11). The verification-strategy decision of the reuben MCP server effort —
wayfinder ticket [MCP/K (#286)](https://github.com/Impractical-Instruments/reuben/issues/286)
on map [#270](https://github.com/Impractical-Instruments/reuben/issues/270). **Rides on**
[ADR-0019](0019-performance-benchmarking.md) (the iai instruction-count CI gate and
bench-history), [ADR-0044](0044-mcp-stdio-sidecar.md) (the structure channel's home in `play`),
and [ADR-0046](0046-coordinator-swap-engine-unit.md) (Coordinator/Swap, the M1 restart-swap /
M2 mailbox-swap split, and the four-verb channel this ADR tests); **amends none**. Feeds the
per-ticket verification criteria [MCP/J (#280)](https://github.com/Impractical-Instruments/reuben/issues/280)
stamps on every implementation-epic ticket.

## Context

The epic (MCP/J) needs a verification answer for every ticket it files, and the map's Notes
name no existing convention for this repo's newest, least-precedented surface: a request/response
protocol channel, and a live-audio swap whose correctness is a *behavioral* property (does state
survive a rewire) rather than a structural one (operator state is opaque — `Box<dyn Operator>`,
no introspection trait). Two conventions already exist elsewhere in this repo and set the shape
of what's reusable:

- **Golden-snapshot pinning with a bless workflow** (`tests/descriptor_golden.rs`,
  `REUBEN_BLESS=1 cargo test ...`): pins a canonical serialization, reds CI on drift.
- **Allocation-counting integration tests** (`tests/rt_safe.rs`): a process-global counting
  `GlobalAlloc`, one test binary per scenario (cargo compiles each `tests/*.rs` file as its own
  binary, so the counter isn't perturbed by concurrently-run sibling tests), asserting zero
  heap allocation across steady-state render.
- **The iai instruction-count CI gate** (`benches/micro_iai.rs`/`macro_iai.rs`, ADR-0019):
  CPU-independent, byte-stable instruction counts via callgrind, trending performance
  regressions on cost-sensitive hot loops.
- **CI's device boundary** (`.github/workflows/ci.yml`'s `windows` job): a headless runner
  can't open a real audio device (WASAPI, ALSA); anything that must actually open a stream is
  verified by a manual smoke pass on real hardware, never in CI.

ADR-0046 splits Swap into two shapes with different testability: M1's restart-swap is
stop-the-world (tear down cpal streams, `Engine::from_document`, reopen — real device I/O, no
survivors by construction) while M2's mailbox swap is a headless-reachable operation (§7: the
Coordinator is a passive, OS-free `reuben_core::coordinator`; §6: Swap never touches devices).
That split is why this ADR's answers differ by milestone rather than giving one blanket answer.

## Decision

### 1. The structure channel: live-server integration test, golden-pinned responses

A `reuben-native` integration test starts the real structure-channel server (the `play`-side
module owning the `Coordinator`) in-process — no cpal, since the Coordinator itself is
device-free — connects a raw TCP client, and drives it through all four verbs
(`ping`/`swap`/`get_document`/`get_diagnostics`) against a canned document. Response shapes
(`SwapReport`, diagnostics JSON) are pinned as golden fixtures, reusing the
`descriptor_golden.rs`/`REUBEN_BLESS=1` convention, so wire-format drift reds CI the way
descriptor drift already does. This proves the server's actual TCP/NDJSON framing and verb
dispatch, not just that the serde types round-trip.

**Considered and rejected:** *golden fixture pinning alone, no live server* — proves the wire
types serialize correctly but not that the server's request-dispatch, framing, or connection
handling work; a protocol bug (e.g. NDJSON line-splitting) would pass a types-only test and
fail in the field.

### 2. Swap correctness off-device: Coordinator-direct, behavioral assertions

A `reuben-core` integration test drives `Coordinator`'s API directly, bypassing the TCP channel
(the channel's own correctness is decision 1's job) — manually invoking the same RT-side
install-check function the audio callback calls, then rendering blocks through the resulting
`Engine`. Because operator state is opaque (ADR-0046 §4: "the operator instance *is* the
state," no extraction trait), survivor assertions are behavioral: swap in a document that
rewires an already-decaying envelope's neighbors and assert the output continues decaying
smoothly (no re-attack transient); swap in a document that bumps `voices` on the same address
and assert the pool resets (old held notes silent, fresh voice count) — the exact two cases the
ticket named.

**Considered and rejected:** *routing this harness through the TCP channel too* — would prove
channel→Coordinator wiring in the same test, but is redundant with decision 1's harness and
conflates two failure modes (channel framing vs. migration-table correctness) in one assertion.

### 3. Swap RT-safety in CI: allocation-counting, not a bench gate

The callback-side install step (mailbox check + migration-table pointer-swap loop) gets an
allocation-counting assertion, reusing `rt_safe.rs`'s process-global counting `GlobalAlloc`
pattern — wrapping the install step of decision 2's harness in a before/after counter check.
This is a hard, binary correctness invariant (RT-safety = zero allocation), which
allocation-counting proves directly. No new iai/bench case is added for this: the install path
is a fixed, small, bounded operation — the invariant that matters is binary (zero-alloc or
not), not a trend worth watching for gradual drift the way an operator's DSP hot loop is.

**Considered and rejected:** *an iai instruction-count case for the install path* — the right
tool for catching gradual performance drift on cost-sensitive hot loops, but overkill for a
step whose correctness bar is pass/fail zero-allocation, not a trend line; revisit only if the
migration table walk grows a data-dependent cost shape worth trending.

### 4. M1 restart-swap: automated contract coverage + manual device-level ritual, split

Not "manual, full stop." The `swap` verb's request/response contract in M1 mode — validation,
error reporting, `SwapReport` shape — doesn't touch a device (ADR-0046 §6: Swap never touches
devices, even in M1's stop-the-world form), so it rides on decision 1's headless live-channel
test. What can't be asserted headlessly is the actual stream teardown/reopen and the audible
gap, because CI can't open a real audio device — this repo's own `windows` CI job already draws
this exact line (build+test only; device behavior verified by manual smoke pass on real
hardware). So: automated coverage for the swap-verb contract, manual smoke-test ritual for the
device-level behavior (does the ~100ms gap sound like the documented rudeness, does the
transport actually resume).

**Considered and rejected:** *treating the whole of M1 restart-swap as manual, undifferentiated*
— would forgo automated coverage of the report-contract/error-path surface that doesn't need a
device at all, leaving CI blind to a class of regressions decision 1's harness can already catch.

### 5. The #220 demo bar: scripted setup, human judgment

The epic's top-level acceptance criterion ("make the bass rounder and add a dub delay" → hear
it change without the transport stopping) stays a human ritual, but a *scripted* one: a fixed
starting instrument document and the exact prompt text to hand the agent are checked in, so the
scenario reproduces run to run. What stays human is the judgment call — does it sound rounder,
is the dub delay present, did the transport visibly/audibly not stop — because those are
perceptual and this is deliberately the highest bar sitting above every automated per-ticket
gate.

**Considered and rejected:** *an LLM-judged audio diff (feeding before/after render buffers to
a judge model) to make the demo fully automatable* — over-engineering the epic's one
deliberately-human acceptance ritual; "rounder" and "the transport didn't audibly stop" are the
exact perceptual judgments a human ear is the right final check for, not a case where
automation was unreliable and needed scripting per decision 6.

### 6. Standing principle for the rest of the epic: scripted human tests where automation can't reach

Generalizing beyond this ticket's five questions: wherever a per-ticket verification criterion
can't be reliably automated (device-level audio behavior, perceptual judgment), MCP/J stamps a
**scripted** human test — precise instructions plus copy/paste commands or prompts — rather than
an unscripted "go check it manually." Decisions 4 and 5 are both instances of this; MCP/J
applies it to every future epic ticket that lands in the same spot.

## Consequences

- The epic's per-ticket verification criteria (MCP/J's job) now have a concrete menu to stamp
  from: live-server + golden-pinned integration tests (channel-shaped work), Coordinator-direct
  behavioral harnesses (swap-correctness work), allocation-counting tests (RT-safety
  invariants), and scripted human rituals (anything device-level or perceptual that automation
  can't reach).
- `reuben-core` gains two new integration-test shapes: a Coordinator-direct survivor/reset
  harness (decision 2) and its allocation-counting companion (decision 3) — both land with the
  M2 tickets that build `reuben_core::coordinator` (ADR-0046 §7), since they exercise machinery
  that doesn't exist until then.
- `reuben-native` gains a structure-channel live-server integration test with a golden-fixture
  file under its `tests/golden/` tree (decision 1), landing with the M1 channel-module ticket
  (ADR-0046 §10) and extended, not replaced, when M2's mailbox swap lands.
- No new CI job or bench workload is added by this ADR — decisions 1-3 all run inside the
  existing `check` job's `cargo test --workspace` step; decision 3 reuses `rt_safe.rs`'s pattern
  rather than the `bench` job's iai gate.
- M1's restart-swap and the #220 demo bar each get a checked-in script (fixed document +
  fixed prompt/commands) that MCP/J's tickets link to, rather than a free-form "verify this
  manually" instruction.

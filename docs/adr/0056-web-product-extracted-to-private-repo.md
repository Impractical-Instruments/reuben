# ADR-0056: The web product is extracted to a private AGPL repo; this repo is the SDK

## Status

Accepted (2026-07-12). The public-side record of the extraction epic
([#414](https://github.com/Impractical-Instruments/reuben/issues/414)), realized here by WX-14
([#432](https://github.com/Impractical-Instruments/reuben/issues/432)). The private repo's own
records are
[reuben-web ADR-0001](https://github.com/Impractical-Instruments/reuben-web/blob/main/docs/adr/0001-move-browser-shell-wholesale.md)
(the whole-crate move) and
[ADR-0002](https://github.com/Impractical-Instruments/reuben-web/blob/main/docs/adr/0002-deploy-cutover-branch-strategy.md)
(its deploy cutover).

**Supersedes** [ADR-0041](0041-web-player-app-in-repo.md) — the player app no longer lives at
`/web`, or anywhere in this repo — and **restores** the clause of
[ADR-0026](0026-v1-finish-line-osc-out-and-stereo.md) that ADR-0041 had retired: a dedicated UI
belongs to a consuming application in its own repo. **Supersedes**
[ADR-0054](0054-web-chat-agent-host.md) as a *public* decision (the chat agent host, its proxy,
its key, and its cost ceiling are now product decisions of a repo this one cannot see), keeping
only its §3 invariant that the tool contract is generated from `reuben-core`'s types.
**Supersedes in part** [ADR-0042](0042-share-links.md) — the codec and the links are private; §3's
`hound` trust boundary remains an obligation on public core.

**Amends** [ADR-0055](0055-dev-staging-branch-strategy.md) §1–§2 (its `web`/`webapp` CI lanes and
its Cloudflare staging alias move out; §3–§6 — the fast-forward promotion model, the App-token
push, no-direct-commits-to-`main`, and the `main`-keyed bench/release machinery — stand untouched),
[ADR-0040](0040-raw-c-abi-worklet-boundary.md) (the *shell* left; the C-ABI *contract* stays and is
now the public browser story), [ADR-0052](0052-web-parity-contract-not-protocol.md) (§2/§3's in-page
tool layer is private; §5's "contract types live OS-free in core, one schema many doors" is still a
public-core constraint), [ADR-0043](0043-surface-docs-decouple-presentation-from-instruments.md)
(the twin resolvers now live in different repos, pinned by a fixture committed here), and
[ADR-0049](0049-no-resource-bytes-over-mcp.md) (its browser-embedded consumer is now cross-repo).

**Rides on** [ADR-0039](0039-engine-in-core-embed-surface.md), which is not amended but *promoted*:
the embed surface is now the entire public embedding story. **First record of the BSD/AGPL split.**

## Context

The extraction epic originally drew the seam *inside* `crates/reuben-web`: the browser shell (wasm
C-ABI, engine JS, surface renderer, share codec) would stay here under BSD and be consumed by the
private repo, while only the chat-agent layer went private. That seam was **real** — the engine
layer imports nothing from the agent layer — but it was not free. `js/` interleaved fourteen engine
files with eleven agent files, so it was a file-by-file split rather than a subtree move;
`tool_schema.rs` had to be carved into a host-only crate and guarded; and every engine-JS or
surface-renderer edit would pay a public-branch-plus-submodule-bump tax forever.

That tax buys one thing: a reusable **BSD browser SDK**. We do not want to own one. `reuben-web`
was a workspace-detached leaf crate with exactly one consumer — this app — and no intent to
support a second. Keeping it public would have committed us to maintaining a public browser
binding as an API, with the compatibility obligations that implies, in exchange for no user we
actually have.

**So the deciding factor is support surface, not licensing.** The AGPL boundary and the
support boundary happen to fall in the same place, but if the licence question had gone the other
way we would still have moved the shell out, because the thing we decline to maintain is a public
browser SDK — not a public copyleft one.

## Decision

### 1. The product moves; the SDK stays

The whole `crates/reuben-web/` crate **and** the `web/` app leave this repo, wholesale, and land in
the private `reuben-web` repo at their original paths.

**This repo is the SDK:**

| Stays here (BSD) | Why |
|---|---|
| `reuben-core` | the engine + the **embed surface** (ADR-0039) + the C-ABI contract (ADR-0040) |
| `reuben-native` | the `reuben` CLI and its audio/OSC/filesystem host |
| `reuben-mcp` | the stdio MCP sidecar (ADR-0044) |
| `instruments/`, `surfaces/` | the instrument library and presentation docs — SDK fixtures, load-bearing in core tests |

**Leaves (AGPL):** the browser shell, the player app, the chat-authoring agent, the relay proxy,
`toys.json` curation, and the product design docs.

**Considered and rejected:** splitting `crates/reuben-web` along the engine/agent seam and keeping
the engine half public. Rejected for the reason in Context — it preserves a public browser SDK
nobody asked us to support, and charges a permanent two-repo tax on every edit to it.

### 2. The licence boundary is the repo boundary

This repo stays **BSD-3-Clause**. The private repo is **AGPL-3.0** — copyleft over both the served
client and the network relay, with a dual-licence option preserved. No file is dual-headed; the
split is by repository, so "which licence governs this code" is answered by "which repo is it in."
This is the first ADR to record the split; nothing before it said which licence anything was under.

### 3. The private repo consumes this one as a submodule

`reuben-web` pins this repo at `engine/` as a git submodule and builds against
`reuben-core` through a Cargo path dependency into it. Engine changes land **here**, on a normal
branch and PR, and the private repo bumps its pinned SHA to adopt them. The wasm is built from the
private crate; only its `reuben-core` dependency reaches into the submodule.

The submodule pin is the version boundary between the two repos, which makes the engine version a
property of the *pin*, not of a shared build. (ADR-0042 §4 already observed that a share link makes
the sender's engine version a property of a URL; it is now also a property of a cross-repo SHA.)

### 4. What has to stay public *because* the split happened

The extraction's one genuinely load-bearing casualty was the **surface oracle**. ADR-0043 §9 pins
the two resolvers — the Python TouchOSC emitter here and the JS twin in the web repo — to identical
output by a shared fixture, and that fixture lived inside the crate that left.

The fixture is therefore promoted to a **public SDK fixture** at
`surfaces/testdata/expected-widgets.json`, the same treatment `instruments/` and `surfaces/` get.
The private JS twin regenerates it through its `engine/` submodule; the public Python suite asserts
against it directly. **The cross-implementation pin survives the split, and now spans the two
repos.**

This was the trap worth naming: the public suite guarded the fixture with
`skipUnless(ORACLE.exists())`. Deleting the crate would have left CI **green** while ADR-0043 §9's
guarantee silently evaporated. The guard is now a loud failure — an absent oracle is a broken repo,
not a not-yet-landed state.

**Considered and rejected:** duplicating the fixture in both repos. Two copies with nothing to
catch their divergence is precisely the failure an oracle exists to prevent.

### 5. The C-ABI, not the shell, is the public browser story

A third party who wants reuben in a browser does **not** get a maintained binding from us. They get
`reuben-core` compiled to `wasm32-unknown-unknown` (which it does untouched) and the documented raw
C-ABI worklet boundary of ADR-0040 — one `Engine::fill` per quantum, fetch-on-miss resource
staging, a flat tagged control channel — from which the binding is reconstructible. That the shell
was thin enough to be worth rebuilding is exactly why we felt free to stop publishing it.

**Trade-off accepted:** there is no longer a ready-made BSD browser binding, and someone who wanted
one now has work to do. We judge the C-ABI stable and documented enough to carry that weight.

## Consequences

- The public CI loses the `web`, `webapp`, `deploy-web`, and `web-chat-live-eval` jobs, their path
  filters, and the Cloudflare deploy. `ci-passed` no longer gates on them. No public job spends
  Anthropic tokens or touches a Cloudflare account.
- `surfaces/**` joins the `check` job's path filter: the oracle used to ride the deleted `web`
  filter, and without it an oracle or surface-doc edit would leave the only suite that reads it
  Skipped.
- The MSRV lockstep script loses its detached-crate loop — `reuben-web` was the only detached
  crate, and every remaining crate inherits `rust-version` from the workspace.
- ADR-0041, ADR-0042, ADR-0054 and the web halves of ADR-0040/0043/0049/0052/0055 remain in this
  repo **as history**, marked at the top with what this ADR retires. They record why the app was
  built the way it was, which is still true — it just isn't built here.
- The product design docs (`docs/web-chat-authoring-ux-spec.md`, the web-chat rituals) move to the
  private repo. The ADRs do not: they are this repo's decision history, and history does not
  relocate.
- ADR-0026's packaging inference — a dedicated UI belongs to a consuming application in its own
  repo — is **vindicated**. ADR-0041 retired that clause on the strength of three arguments (the
  engine already reached into the browser from here; this was the only place it could be built and
  deployed; "own repo" bought isolation the app didn't want because it must track the engine
  lockstep). The first two were artifacts of not yet having a submodule; the third is answered by
  the pin. The clause is restored.

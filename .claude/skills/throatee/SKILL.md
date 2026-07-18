---
name: throatee
description: Relentless audit of a target for code that has stopped earning its keep — reports impact-ranked deletions and simplifications, and stops at the report. Audit-only, never fix; invoke it by name.
disable-model-invocation: true
---

# throatee

**Code is a liability, not an asset — less code is better.** throatee is a relentless, brutally honest
minimalist auditor. It hunts a target for code that has stopped earning its keep and reports one of two
moves: **cull** it (DELETE) or **collapse** it (SIMPLIFY). Values rank **correctness > maintainability >
performance** — ideally a finding wins all three; when they conflict, throatee says which way it leans in
plain words.

## The one law: audit-only, never fix

**throatee's deliverable is a report. A human takes it into a *separate* session to act.** throatee may
read, grep, run the compiler, write throwaway tests, delete code in a scratch branch or worktree, even
commit — all as **investigation**, clearly labeled as such. But it stops at the report: it **never opens
a PR, never presents a change as "the fix," never files the issues.** Report first; the human decides.
Hold this line even when the fix looks trivial — the slide from audit straight to fix is the exact
failure this skill exists to prevent.

## Step 0 — Scope the run

Audit the target the user named — a crate, module, directory, or feature. **On a bare invocation, ask
first**, two questions only: *what's the target?* and *anything to focus on or leave alone?* "The whole
workspace" is a valid answer; take it crate-by-crate so each pass stays deep rather than skimming
everything at once. Discover the layout live (`cargo metadata`, `ls crates/`, the workspace `Cargo.toml`)
— never assume or hardcode it.

## Step 1 — Build the reference picture (the compiler is your first oracle)

Establish *what calls what* before judging anything. The compiler is the oracle for Rust; for skills,
docs, and scripts it is the reference/invocation graph (Step 2) — build whichever fits the target.

- **Start from what `rustc` already flags.** Build the target and read its `dead_code` / `unused`
  warnings — free, proven findings. An `#[allow(dead_code)]` or `#[allow(unused)]` is itself a red flag:
  someone silenced the compiler; ask why.
- **Map the callers of each suspect across the whole workspace** — and across a **reachable downstream**
  when one exists. *Detect* it, don't go hunting: `git rev-parse --show-superproject-working-tree` names
  the superproject when this repo is checked out as its submodule — the shape a consumer takes (it
  submodules this repo, and may symlink its skills). Scan that tree too; for a consumer not on disk, rely
  on the user saying so. The workspace is not the world — and "exported" isn't only a `pub` item: a
  skill, doc, or script can have downstream consumers too.
- **Tests never count as usage.** A caller under `#[cfg(test)]`, in `tests/`, or in `benches/` does not
  keep production code alive.

## Step 2 — Hunt, on two spines

**Both spines run every audit** — a DELETE-only pass is a half-audit. Report zero findings on a spine
honestly when the code is clean, but never skip the sweep.

**DELETE — cull it entirely:**
- **Dead code** — no caller anywhere but tests.
- **Vestigial code** — *production* code (shipped in the build, not `cfg(test)`) whose only callers are
  tests. This is the prime target: code that exists only to be tested. "But a test needs it" never
  launders it — that *is* the smell.
- **Whole unused features** — a subsystem never wired into anything real.
- **Unused dependencies** — a `Cargo.toml` entry nothing references (use `cargo-machete` / `cargo-udeps`
  if present, else scan references by hand).
- **Commented-out code** — dead by definition.

**SIMPLIFY — collapse it, less code same behavior.** Hunt the high-signal, cheap-to-spot tells rather
than reading every line — that keeps this spine tractable enough to run by default:
- **Needless indirection** — a trait with one impl, a wrapper/newtype that only forwards, a generic with
  a single concrete caller, a function called from exactly one place that just wraps another.
- **Duplication** — the same block or shape in two places that could unify.
- **Dead knobs** — config, features, parameters, or match arms nothing ever sets to more than one value.

**Guardrails — rule these out before you cull, or you cry wolf:**
- **Indirect dispatch hides callers.** Registries (compile-time self-registration), trait objects,
  macro-generated wiring, serde, FFI, handlers dispatched by name — "no direct caller" ≠ dead. Rule out
  indirect invocation first; `rustc` staying *silent* on an item demands more suspicion, not less.
- **Public API is aggressive-but-flagged, and the workspace is not the world.** Before flagging an unused
  `pub` item — or an unreferenced skill, doc, or script — scan the reachable downstream (Step 1) and treat
  a consumer found there as a live use. For a consumer you *cannot* reach (not on disk, or published),
  flag it but **cap at Probable** and name the exact repo/path to grep — never `Certain` on an in-repo
  scan alone.
- **Legitimate test infrastructure is not vestigial.** Anything `cfg(test)` or in a test directory is
  *supposed* to be test-only; leave it. The target is production code kept alive *only* to be tested.

**Non-code targets — the skills, docs, and scripts are cruft havens too.** Apply the same two spines to
them: a whole skill or doc section can be vestigial, a script unreferenced. Here the oracle is the
**reference/invocation graph** — is anything loading, calling, or linking it — plus the no-op /
duplication / sediment lens. Own the question *does this earn its existence*; leave *is this accurate* to
[`sync-docs`](../sync-docs/SKILL.md).

## Step 3 — Earn the confidence (correctness is the brake)

Every finding carries a confidence level. Promote it with the cheapest sufficient evidence; escalate to a
**cull-experiment** — remove the code in a scratch branch/worktree, run `cargo check` + `cargo test` —
only to lift a candidate that earns it, typically a likely top recommendation, not every grep hit.

- **Certain** — compiler and reference graph agree, or a cull-experiment stayed green (proving no caller
  *in this workspace*, generated code included). An **exported** thing — a `pub` item, or a skill / doc /
  script a downstream could consume — earns `Certain` only once the reachable downstream is scanned too,
  else it caps at Probable.
- **Probable** — strong signal, one residual unknown, named (e.g. unused `pub`, indirect dispatch not
  fully ruled out).
- **Possible** — a real smell that needs human judgment.

**Correctness is the brake.** Removing genuinely dead code can't hurt it — clean win, no caveat. A
*simplification* that could change behavior carries correctness risk: never advocate a change you believe
breaks behavior; if you can't convince yourself behavior is preserved, say so and lower the confidence
rather than dropping the finding. Maintainability-vs-performance conflicts are always surfaced with an
explicit **tradeoff** line — the value ranking tells the reader which way you lean; it never buries the
option.

## Step 4 — Report (the deliverable)

Write the full report to `target/throatee/<target>-<date>.md` — gitignored, so it survives to the
fix-session yet can never become committed cruft — and print the executive summary to the conversation.
Order the detailed findings **most-certain-first**. Rank the executive top-4 by **impact**, biggest win
first, with confidence as an annotation. **Up to four; never pad to reach four** — a manufactured fourth
is the noise this skill deletes. Close by suggesting the human turn the recommendations into issues, but
do not file them.

### Report format

```
# throatee audit — <target> — <date>

## Executive summary
Up to 4 recommendations, ranked by impact, in plain language a non-programmer feels.
Each line: what to do · what it buys (developer velocity / performance / build time /
dependency count / binary size / surface area) · confidence · what to double-check if not Certain.

## Findings  (most-certain-first)
- `file:line` · DELETE|SIMPLIFY · Certain|Probable|Possible · <one-line finding>
  Evidence:     <what rustc said / what the reference graph showed / cull-experiment result>
  Tradeoff:     <only when values conflict>
  Double-check: <only when not Certain — the exact thing a human must confirm>

## Coverage
Swept — DELETE: <what was swept> · SIMPLIFY: <what was swept>  (both spines, every audit).
Not descended into: <the gaps> — so no silent "all clear."

## Next step
These are findings, not changes. Consider filing them as issues; fixing is a separate session.
```

**Confidence vocabulary:** `Certain` · `Probable` · `Possible`.
**Spine vocabulary:** `DELETE` (cull) · `SIMPLIFY` (collapse).

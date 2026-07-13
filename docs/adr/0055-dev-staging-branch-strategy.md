# ADR-0055: `dev` staging branch + promotion-based production release

> **Amended by [ADR-0056](0056-web-product-extracted-to-private-repo.md).** §1's `web`/`webapp` CI
> lanes and §2 entirely (the Cloudflare Pages project and its `dev` branch alias) moved out with the
> web player; the private repo re-decides its own deploy in
> [reuben-web ADR-0002](https://github.com/Impractical-Instruments/reuben-web/blob/main/docs/adr/0002-deploy-cutover-branch-strategy.md).
> **§3–§6 stand untouched** and still govern this repo: `dev` is the integration branch, production
> ships by fast-forward promotion (never a merge button), the promotion push is authored by a GitHub
> App token, no direct commits to `main`, and the `main`-keyed bench-history/perf/release machinery
> is unchanged. Pushing to `dev` still runs full CI here — it just no longer deploys anything.

## Status

Accepted (2026-07-11). Introduces a long-lived integration branch and a staging environment for the
web player, resolving issue [#346](https://github.com/Impractical-Instruments/reuben/issues/346).
**Rides on** [ADR-0041](0041-web-player-app-in-repo.md) (the `/web` player app and its Cloudflare Pages
deploy), [ADR-0019](0019-performance-benchmarking.md) (the iai perf gate and `bench-history`, which
stay `main`-only), and the branch-restriction rulesets. **Amends none** — production still ships off
`main`; this adds a bake stage in front of it.

## Context

The repo was trunk-based: `main` is the Cloudflare Pages **production** app, and every PR got an
**ephemeral preview** (`deploy-web`, `--branch=<head>`). There was no long-lived integration branch
and no persistent staging environment, so PR'd-but-possibly-risky work reached production the moment
it merged — the only pre-production signal was the throwaway per-PR preview, which vanishes on merge.

We want a place for merged work to bake against a stable, shareable URL before it reaches production,
**without** standing up a second Cloudflare project or a second set of deploy secrets, and without
disturbing the parts of CI that are deliberately `main`-keyed (the production branch, `v*` release
tags, and the `bench-history` perf trend).

## Decision

### 1. `dev` is the default, long-lived integration branch; all PRs target it

A single `dev` branch, created off `main` and set as the repo default. Every PR targets `dev`.
Pushing to `dev` runs the full CI suite (`check`, `windows`, `web`, `webapp`, `bench`) and deploys a
**staging** app. This is one line in `ci.yml`: `push: branches: [main, dev]`. The existing per-job
path filters, draft skips, and per-PR previews are unchanged.

**Considered and rejected:** *keep trunk-based, rely on per-PR previews* — a preview is ephemeral and
per-change; it can't serve as a stable integration target where several merged changes bake together
against one shareable URL, which is the whole point.

### 2. Staging is a same-project Cloudflare **branch alias**, not a new project

`deploy-web` already passes `--branch=${{ github.head_ref || github.ref_name }}`, which on a push to
`dev` resolves to `dev` → Cloudflare serves it at `https://dev.reuben-web-player.pages.dev`, a branch
alias of the *existing* `reuben-web-player` project. No new project, no new secrets, no dashboard
change — branch aliases are automatic. Production stays the project's production branch (`main`).

**Considered and rejected:** *a separate `reuben-web-player-staging` Pages project* — doubles the
secret surface and the SETUP ritual for zero benefit; the same project's branch-alias mechanism gives
a stable staging URL for free.

### 3. Production ships by **fast-forward promotion**, run as a workflow — not a merge button

Promotion is a manual `workflow_dispatch` (`promote.yml`) that does a true `git merge --ff-only` of
`dev` into `main` and pushes. A real fast-forward **preserves SHAs**, so the two long-lived branches
never diverge. No GitHub merge button does a true ff: "Rebase and merge" **rewrites** SHAs, which
would diverge `dev` from `main` on every promotion and break `--ff-only` thereafter. Hence a
workflow, not a button.

**Considered and rejected:** *GitHub's "Rebase and merge" button* — rewrites commit SHAs, diverging
the branches on a long-lived pair where SHA identity is exactly what keeps `--ff-only` working.

### 4. The promotion push is authored by a **GitHub App token**, not `GITHUB_TOKEN`

`promote.yml` mints a token from a GitHub App (or machine account) holding `contents: write`, stored
as repo secrets `PROMOTE_APP_ID` / `PROMOTE_APP_PRIVATE_KEY`. `GITHUB_TOKEN` cannot do this push for
**two** independent reasons, either of which alone is disqualifying:

1. `main`'s ruleset **restricts pushes** to a bypass list; `GITHUB_TOKEN` isn't on it.
2. A push authored by `GITHUB_TOKEN` does **not** trigger downstream workflows — so the `push:
   [main]` CI → `webapp` → `deploy-web` (production) chain would silently never fire, and production
   would never actually deploy.

The App is the sole entry on `main`'s ruleset bypass list, so its push both lands and triggers the
production deploy.

**Considered and rejected:** *`GITHUB_TOKEN` with elevated `contents: write`* — clears neither
constraint above; the ruleset still blocks it and its push still fires no downstream deploy.

### 5. No direct commits to `main`

Anything on `main` not reached from `dev` diverges the branches and breaks `--ff-only` until `main`
is merged back into `dev`. The hotfix rule: route through `dev`; if a fix truly must land on `main`
directly, immediately `git merge main` into `dev` to restore fast-forwardability. `main`'s ruleset
deliberately does **not** add "require a pull request" — that would block the ff promotion push
itself.

### 6. `main`-keyed machinery is untouched

`bench-history` persistence and the perf-history concurrency special-case already key on
`refs/heads/main`, so they stay production-only by construction: the perf trend records one point per
promotion batch, not one per `dev` push. Release tags (`v*` off `main`) and the Cloudflare production
branch are likewise unaffected.

## Maintainer setup (the durable record of issue #346, Part 1)

These are GitHub-admin / secret-provisioning steps that cannot be done in a code PR. Recorded here so
the model can be reconstructed:

1. **Create `dev` off `main` and push it** before merging the CI-trigger change, so the first `dev`
   push exercises the new path: `git branch dev main && git push -u origin dev`.
2. **Set `dev` as the default branch** (Settings → General, or the API).
3. **Create/install a GitHub App** (or machine account) with `contents: write` on this repo; store
   its credentials as repo secrets `PROMOTE_APP_ID` and `PROMOTE_APP_PRIVATE_KEY`.
4. **Configure two rulesets:**
   - **`dev`:** require a pull request before merging **+** require the `ci-passed` status check.
   - **`main`:** restrict pushes, bypass list = the promotion App **only**. Do *not* add "require a
     pull request" to `main` (it would block the ff promotion push).
5. **Set `CLOUDFLARE_API_TOKEN` + `CLOUDFLARE_ACCOUNT_ID`** (existing secrets). Both production and
   the staging alias ride `deploy-web`'s self-gate, so they stay dormant-green until these exist.

> The `ci-passed` required check (step 4) can only be selected in the `dev` ruleset **after** the
> Part 2 PR has run CI at least once — a required status check must have been observed before a
> ruleset will offer it. Configure the rest of the `dev` ruleset first, land Part 2, then add the
> check.

After the default-branch switch, contributors run `git remote set-head origin -a` once so
`origin/HEAD` (and `scripts/clean-merged-branches.sh`, which auto-targets the default branch) follow
`dev`.

## Consequences

- **`ci.yml` grows an always-green `ci-passed` aggregate job.** A ruleset can't require the
  per-path-filtered jobs directly (a Skipped job reads as an unmet "expected but missing" check), so
  the `dev` ruleset requires this one aggregator, which passes unless a required upstream actually
  failed or was cancelled. `deploy-web`/`bench-history` are deliberately excluded — they're
  post-merge side-effects, not merge gates.
- **A new `promote.yml`** carries the manual production release. Its first step fails loud if the App
  secrets are absent; the ff step fails loud with the reconciliation command if `main` has diverged.
- **Staging deploys automatically** on every `dev` push once the Cloudflare secrets exist; no code
  change flips it on.
- **Deliberately unchanged:** `scripts/clean-merged-branches.sh` (auto-targets the default branch via
  `origin/HEAD`, already protects `dev` — dev-aware for free), `web/scripts/gen-share-links.mjs`'s
  `ORIGIN` (stays the production origin by design — committed README links must not vary by
  environment), the Cloudflare project/dashboard (branch aliases are automatic), and `bench-history`
  / the perf gate / `release.yml`.

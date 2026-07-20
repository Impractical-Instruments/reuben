# Why: Production ships only by fast-forward-only promotion of `dev` onto `main`, run as a workflow authored by a GitHub App token, with no direct commits to `main`.

[Rule](../../web-product-process.md#ff-promotion-to-main)

Promotion is a manual `workflow_dispatch` that does a true `git merge --ff-only` of `dev` into `main`
and pushes. A real fast-forward **preserves SHAs**, so the two long-lived branches never diverge —
which is the whole point, because no GitHub merge button does a true ff. "Rebase and merge"
*rewrites* SHAs, which would diverge `dev` from `main` on every promotion and break `--ff-only`
thereafter. Hence a workflow, not a button; and hence **no direct commits to `main`** — anything on
`main` not reached from `dev` diverges the pair and breaks fast-forwardability until `main` is merged
back into `dev`. `main`'s ruleset deliberately does *not* "require a pull request", because that would
block the ff promotion push itself.

The push is authored by a **GitHub App token**, not `GITHUB_TOKEN`, for two independent
disqualifying reasons: `main`'s ruleset restricts pushes to a bypass list that `GITHUB_TOKEN` is not
on; and a push authored by `GITHUB_TOKEN` does not trigger downstream workflows, so the production
deploy chain would silently never fire. The App is the sole entry on the bypass list, so its push
both lands and triggers downstream. This model is unchanged by the web extraction — only what
"downstream" deploys changed. The `main`-keyed perf-history and release machinery ride on it
untouched: they key on `refs/heads/main`, so the perf trend records one point per promotion batch,
not one per `dev` push. See [dev-integration-branch](dev-integration-branch.md) for the bake stage
this promotes from.

Distilled from: ADR-0055

# Why: `dev` is the default long-lived integration branch that every PR targets, and every push to it runs the full CI suite.

[Rule](../../web-product-process.md#dev-integration-branch)

The repo was trunk-based: `main` was production and the only pre-production signal was a throwaway
per-PR preview that vanished on merge, so PR'd-but-risky work reached production the moment it
merged. A single long-lived `dev` branch, created off `main` and set as the repo default, gives
merged work a place to bake together against a stable target before promotion. Every PR targets
`dev`; pushing to `dev` runs the full CI suite. A per-PR preview was rejected as the integration
point precisely because it is ephemeral and per-change — it cannot be the one stable place several
merged changes accumulate.

`dev` used to also deploy a staging web app (a same-project Cloudflare branch alias); that lane left
with the web product, so a push to `dev` now runs full CI and deploys nothing. What survives is the
branch's role as the integration and bake stage in front of the fast-forward
[promotion to `main`](ff-promotion-to-main.md). The `dev` ruleset requires a PR and a single
`ci-passed` aggregate check — an aggregator is needed because a path-filtered job that Skips reads to
a ruleset as an unmet "expected but missing" check.

Distilled from: ADR-0055

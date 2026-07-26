# Why: `dev` is the default long-lived integration branch that every PR targets, and every push to it runs the full CI suite.

[Rule](../../web-product-process.md#dev-integration-branch)

A single long-lived `dev` branch, created off `main` and set as the repo default, gives merged work a
place to bake together against a stable target before promotion — which a throwaway per-PR preview
cannot be, being ephemeral and per-change rather than the one place several merged changes
accumulate. Every PR targets `dev`; a push to `dev` runs the full CI suite and deploys nothing. The
branch's role is the integration and bake stage in front of the fast-forward
[promotion to `main`](ff-promotion-to-main.md). The `dev` ruleset requires a PR and a single
`ci-passed` aggregate check — an aggregator is needed because a path-filtered job that Skips reads to
a ruleset as an unmet "expected but missing" check.

Distilled from: ADR-0055

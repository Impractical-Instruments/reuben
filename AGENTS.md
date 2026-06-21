# reuben

## Agent skills

### Issue tracker

Issues are tracked in GitHub Issues for `Impractical-Instruments/reuben` via the `gh` CLI. External PRs are not a triage surface. See `docs/agents/issue-tracker.md`.

### Triage labels

Default label vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.

### Authoring

How to build Operators and Instruments — the code-level contract (Operator trait,
descriptor, `spawn`), the JSON format, adding an operator, and the determinism/rt-safe
invariants. The grounding doc the V1.6 operator-authoring and patcher skills lean on. See
`docs/agents/authoring.md`.

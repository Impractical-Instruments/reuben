# Why: Authoring grounding is single-sourced — normative prose lives once in the authoring guide and the skills, CLI, and MCP server point at it (gist-and-point) — while every door descends to the same introspect and loader so facts cannot drift.

[Rule](../../agent-mcp.md#grounding-single-source)

**Code-level drift is structurally impossible; prose drift is the whole battle.** The CLI and the
MCP sidecar descend to the same pure functions in `reuben_core::introspect` and the same loader
([introspection-surface](introspection-surface.md), [loader-single-authority](loader-single-authority.md)),
so the surfaces cannot disagree about facts. What *can* drift is prose — the authoring guide, the
contract-inlining skills, and the server's `instructions` plus tool-description strings. The fix is
single-sourcing by three mechanisms in order of strength: **by construction** where a canonical doc
exists (the authoring guide is the one home for the format rules, the type system, wiring, and the
try-then-commit loop; skills keep their *workflow* and worked examples but drop normative contract
prose in favor of pointers); **by posture** where prose must live in code — **gist-and-point**: the
server strings carry the one-breath gist and point at `reuben://guide/authoring`, because
duplication is a drift pair and a pointer cannot drift; and **by manual sweep only as a backstop**
(`sync-docs`'s narrowed scope).

Two shipping details make the posture real. The guide's mixed audience is split — the authoring
agent's half stays in `docs/agents/authoring.md`, the Rust operator-development half extracts to its
own doc — so an MCP client is not served ~600 lines of Rust internals it cannot act on. And the
sidecar serves its resources by **reading the files from the checkout at request time** (never
`include_str!`), so a binary built yesterday cannot serve yesterday's guide; the MVP persona runs the
sidecar from the repo, so the files are always present (a non-checkout deploy overrides the path by
env var). Transport is partitioned by verb class — no verb is taught two ways: repo skills keep
`describe`/`validate` on the CLI (works in a bare checkout, never stale), and the engine verbs that
have no CLI equivalent are taught only on MCP.

Distilled from: ADR-0051

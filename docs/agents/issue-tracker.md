# Issue tracker: GitHub Issues

<!-- GENERATED from templates/issue-tracker.md + .ii/repo.toml by `python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .` — edit the source, not this file. sha256=afe41d08981a06f6f1ac99569ab132220a0331aad603b68f0903245bc8cd2496 -->

Issues and specs for this repo live as GitHub Issues. Use the `gh` CLI for all operations.

## Conventions

- **Create an issue**: `gh issue create --title "..." --body "..."`. Use a heredoc for multi-line bodies.
- **Read an issue**: `gh issue view <number> --comments`, filtering comments by `jq` and also fetching labels.
- **List issues**: `gh issue list --state open --json number,title,body,labels,comments --jq '[.[] | {number, title, body, labels: [.labels[].name], comments: [.comments[].body]}]'` with appropriate `--label` and `--state` filters.
- **Comment on an issue**: `gh issue comment <number> --body "..."`
- **Apply / remove labels**: `gh issue edit <number> --add-label "..."` / `--remove-label "..."`
- **Close**: `gh issue close <number> --comment "..."`

Infer the repo from `git remote -v` — `gh` does this automatically when run inside a clone.

## Labels

Labels are **all-repo or one-repo, and nothing in between.** The state roles below and the `wayfinder:*` family under "Wayfinding operations" are all-repo: the same strings with the same meanings in every repo this document reaches, because they are shared triage vocabulary rather than a per-repo setting. Anything a repo adds for itself is one-repo, and no other repo needs to know it.

**Label strings are exact.** `gh` matches them literally, spaces and all — a label whose name carries a space is not reachable by the same string without it, and `gh issue edit --add-label` fails rather than guessing. Copy the strings below character for character.

The meanings here are what a label *is*; a label's description field in GitHub is set on the tracker and is not claimed to match.

### State roles

The state axis answers **who acts next**. When an instruction names a role ("apply the AFK-ready triage label"), use the matching label.

- **`ready-for-agent`** — fully specified and safe to run unattended; the dispatcher claims these.
- **`ready-for-human`** — needs a human in the loop, including work an agent could mostly do.

`ready-for-agent` is read by the dispatcher, so applying it starts unattended work. Carrying neither role is a query rather than a third label:

```
is:issue state:open (-label:ready-for-agent AND -label:ready-for-human)
```

### Labels this repo adds for itself

The one-repo case. This repo declares these labels because they are useful here; no other repo needs to know them. The tracker may carry others — stock labels, or ones added by hand; this is the declared set, not an inventory of everything on it.

- `post-v1`
- `design-question`
- `someday`
- `epic`
- `triage`
- `perf`

## Pull requests as a triage surface

**This repo does not treat external pull requests as feature requests.** Triage runs on issues.

## Publishing to the issue tracker

Create an issue. Where the work needs describing completely enough to implement without an interview, its body is a spec.

## Fetching a ticket

Run `gh issue view <number> --comments`. The issue is the container; the spec is what its body says. Not every issue holds one.

## Wayfinding operations

Used when charting an initiative whose shape is not yet known, and working it until the route is clear. The **map** is a single issue; the work hangs off it as **child** issues. This family is all-repo, like the state roles above:

- **`wayfinder:map`** — the initiative map, holding Notes / Decisions-so-far / Fog.
- **`wayfinder:research`** — a child ticket resolved by fact-finding against primary sources.
- **`wayfinder:prototype`** — a child ticket resolved by building a cheap artifact to react to.
- **`wayfinder:grilling`** — a child ticket resolved by stress-testing a decision with a human.
- **`wayfinder:task`** — a child ticket that is straightforward work.

The operations:

- **Map**: `gh issue create --label wayfinder:map`, with the Notes / Decisions-so-far / Fog body.
- **Child ticket**: an issue linked to the map as a GitHub sub-issue (`gh api` on the sub-issues endpoint). Where sub-issues are not enabled, add the child to a task list in the map body and put `Part of #<map>` at the top of the child body. Label it with the `wayfinder:` label that names how it gets resolved — never `wayfinder:map`, which marks the map issue itself and belongs to no child. Once claimed, the ticket is assigned to the driving dev.
- **Blocking**: GitHub's **native issue dependencies** — the canonical, UI-visible representation. Add an edge with `gh api --method POST repos/<owner>/<repo>/issues/<child>/dependencies/blocked_by -F issue_id=<blocker-db-id>`, where `<blocker-db-id>` is the blocker's numeric **database id** (`gh api repos/<owner>/<repo>/issues/<n> --jq .id`, *not* the `#number` and *not* the `node_id`). GitHub reports `issue_dependencies_summary.blocked_by` — open blockers only, which is the live gate. Where dependencies are not available, fall back to a `Blocked by: #<n>, #<n>` line at the top of the child body. A ticket is unblocked when every blocker is closed.
- **Frontier query**: list the map's open children (`gh issue list --state open`, scoped to the map's sub-issues / task list), drop any with an open blocker (`issue_dependencies_summary.blocked_by > 0`, or an open issue in the `Blocked by` line) or an assignee; first in map order wins.
- **Claim**: `gh issue edit <n> --add-assignee @me` — the session's first write.
- **Resolve**: `gh issue comment <n> --body "<answer>"`, then `gh issue close <n>`, then append a context pointer (gist + link) to the map's Decisions-so-far.

## Agent identity

Agent writes to GitHub authenticate as a dedicated agent account, **`ii-impy`**, rather than as a person. That is what makes the tracker's attribution mean something, and it changes how you read what is already there:

- **`ii-impy` on a piece of writing means an agent wrote it.** Its open questions are that agent's inference, never a mandate; its framing is not consent already given. Where a decision is genuinely a human's, ask.
- **`charliehuge` on writing from before 2026-08-07 proves nothing either way.** That writing predates the identity, so the account name does not distinguish an agent's work from a person's. Treat the attribution as unknown rather than as his, and read the content on its merits.

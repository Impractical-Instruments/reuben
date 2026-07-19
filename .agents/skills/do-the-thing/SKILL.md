---
name: do-the-thing
description: Take a GitHub issue all the way to merge-ready PRs — decompose it, fan out Opus implementers in isolated worktrees, pair each with a Sonnet /code-review loop, drive CI green, and report each PR ready for human merge.
disable-model-invocation: true
argument-hint: "<issue number>"
---

# /do-the-thing — issue → merge-ready PRs

You are the **coordinator**. You decompose the issue, then orchestrate agents that do the work: **Opus implementers** write the code, **Sonnet reviewers** run `/code-review`. You never edit code, run `/code-review`, or push commits yourself — every line of that happens inside a spawned agent. Your job is to plan, dispatch, adjudicate, gate, and report.

Repo facts you run against: base branch is `dev`; the one required CI check is **`ci-passed`** (read with `gh pr checks <n>`); issues via `gh issue view <n> --comments`. You are **not** authorized to merge — your terminal state is "ready for human review and merge."

## Phase 1 — Plan, then confirm

1. Read the issue: `gh issue view <n> --comments`. Read `CONTEXT.md` and any relevant `docs/adr/` for grounding.
2. Decide the decomposition by the **independent-mergeability test**:
   > Split into separate PRs **only if** each resulting PR is independently mergeable to `dev` **and** independently `ci-passed`-green — clean, non-overlapping seams with no compile- or test-time dependency between them. Otherwise it is **one PR**.

   **Bias to a single PR.** A split must earn itself; coupled work stays together.
3. Post your proposed decomposition to the user — "single PR" or "N PRs: X / Y / Z" with a one-line scope for each — and **stop. Wait for their go-ahead.**

**Completion criterion:** the user has approved a specific decomposition. Do not spawn anyone before that.

## Phase 2 — Run each PR to *done*

Create one task per PR (`TaskCreate`) and keep it current (`implementing → in-review → ci → ready` / `escalated`) so the user has a live view through the autonomous stretch.

Run each PR's procedure below **concurrently** — launch the independent implementer agents in a single message. Each PR is fully isolated in its **own git worktree** (`isolation: "worktree"`), on branch `claude/issue-<n>-<slug>`, based on `dev`. Isolation is what lets parallel PRs never see each other's changes.

A PR is **done** when the *same* HEAD commit is both **review-clean** (a `/code-review` round returned zero in-scope findings) and **CI-green** (`ci-passed` passing), with nothing changed since. Anything short of that is either still looping or **escalated**.

### 2a. Implement

Spawn the **Opus implementer** (`Agent`, `model: opus`, `isolation: "worktree"`). Keep its agentId — this agent is **persistent**; you resume it with review and CI feedback so it keeps its context. Instruct it to:

- Implement its slice of the issue following the **testing discipline** (below).
- Commit, push `claude/issue-<n>-<slug>`, and open a **non-draft** PR against `dev`. PR body links the issue: **`Closes #<n>`** if this is the only PR, **`Part of #<n>`** if the issue was split (a split PR must not auto-close the issue).
- Report back its PR number, branch, and a short summary of what it did.

**Completion criterion:** the PR exists (has a number) and the branch is on `origin`.

### 2b. Review loop (max 5 rounds)

Each round, spawn a **fresh Sonnet reviewer** (`Agent`, `model: sonnet`) — fresh every round, so it re-reviews adversarially with no "I already approved this" anchoring. Reviewers are **read-only and not worktree-isolated** — they run in the coordinator's main working copy, so they must never mutate shared git state. Instruct it to:

1. `git fetch origin`, then review from a **detached HEAD off the remote**: `git checkout --detach origin/claude/issue-<n>-<slug>`, and diff against `origin/dev` (`git diff origin/dev...HEAD`) so the review diff is the whole PR. **Never** `git checkout <local-branch>`, `git branch --set-upstream-to`, or `git reset --hard` — those move the coordinator's `dev`/branch pointers or fail against the branch already checked out in the implementer's worktree.
2. Confirm the diff under review is non-empty and matches `gh pr diff <n>` — an empty diff means the checkout is wrong, not that the PR is clean. Fix the setup and retry before reviewing.
3. Invoke the code-review skill **exactly once, in the reviewer agent itself**: `Skill(code-review, "high")`. **Do not spawn your own `fork`/`Agent` sub-agents** to parallelize — the skill runs its own internal fan-out, and forked children inherit the reviewer's context (including any posting flag) and duplicate-post to the PR. If the skill stalls waiting on its internal finders, abandon it and review the diff inline instead. **No `--comment`** — return the findings verbatim to the coordinator; do not post to the PR.
4. Return the findings verbatim.

Then **you adjudicate** each finding — you hold the issue and the approved scope:
- **In-scope** → relay to the implementer (resume it via `SendMessage`) to fix, commit, push. Start the next round.
- **Out-of-scope** (real, but outside this issue's scope) → set aside for the report. Do **not** fix it, and do **not** auto-file an issue.

Reviewers never post to the PR — the fix loop (relay → implementer) and the Phase 3 report already carry every finding, so per-round `--comment` only piles stale, since-fixed comments onto the thread. If you want a durable inline trail, **you** (the coordinator) post it **once** at convergence as a single consolidated comment — never fanned out across reviewer agents.

**Review-clean** = a round returns **zero in-scope findings**. Then go to the CI gate.
If round 5 finishes without review-clean → **escalate** (see below).

### 2c. CI gate (max 3 fix attempts)

Once review-clean on the current HEAD, watch CI: `gh pr checks <n> --watch`, capped at **20 minutes**.

- **Green** → the PR is **done** (this HEAD is review-clean *and* CI-green).
- **Red** → pull the failing logs (`gh run view --log-failed`). If it's plainly transient (the `webapp` Playwright smoke is the likely flaky surface; the `crate` tests are deterministic), **re-run once** (`gh run rerun --failed`) *before* spending an attempt. A genuine failure → resume the implementer to fix it — this **costs one of the 3 attempts**. Any fix is a new commit, so it **re-enters the review loop (2b)**: new code must be re-reviewed before it can be done.
- **20-min cap with no verdict, or 3 fix attempts exhausted** → **escalate**.

### 2d. Converge, then clean up

Loop 2b→2c until a HEAD is review-clean and CI-green together. Then:
- Remove the PR's worktree — the branch is on `origin` and CI is green, so the local copy is disposable. Cleanup does **not** wait on the human merge.
- Mark the task `ready`.

### Escalation

When a PR hits a cap (5 review rounds, 3 CI attempts, or the CI wait cap) without converging: **stop that PR's loop**, mark its task `escalated`, and comment the specific sticking point on the PR (the unresolved finding, or the persistent CI failure + relevant log excerpt). **Leave its worktree in place** — it hasn't reached the safe-clean condition, and a human may want to continue in it. Other PRs are unaffected; each converges or escalates on its own.

## Phase 3 — Report

When every PR is `ready` or `escalated`:

- **Chat** — the full summary: **key changes**, **flagged out-of-scope findings** (listed, not filed), **manual-testing instructions** where applicable, and per-PR **ready / escalated** status with links. For escalated PRs, state the sticking point and the worktree path left behind.
- **Issue #<n> comment** — a concise durable status: the PR link(s) and each one's ready/escalated state.
- If the issue was split, remind the human to **close #<n>** once all its PRs merge (the `Part of` links won't auto-close it).

Never merge. Ready-for-human-review is the finish line.

## Reference — testing discipline

Passed to every implementer:

- **New behavior** → `/tdd` (red → green → refactor).
- **Bug fix** → write a failing test that reproduces the bug **first**, then fix until it passes.
- **Refactor** → ensure adequate tests already cover the code **before** refactoring; add them first if they're missing, then refactor under their protection.
- **Non-code** (docs, config, etc.) → no TDD; just keep existing CI green.

CI runs `cargo test`, the node suites, and Playwright smoke — new/changed behavior must be covered by tests that CI actually exercises.

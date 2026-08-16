# ADR-0079 — hooks install as one *set* of per-check fragments, not as a directory

**Superseded in part — the directory only, on 2026-08-12.** Charlie reversed the choice of surviving
directory: the set lives at `scripts/hooks/`, which is the path `brain`'s `bootstrap.sh`
configures, so `scripts/hooks/` is *not* gone and every `.githooks/` path below reads as history. The
reason was that the choice recorded here was agent-authored reasoning treated as settled doctrine
rather than a decision he made. **Everything else this ADR decided stands** — one installed set, a
stub per hook event, one file per check under `<hook>.d/`, and refusing a registry with no stub — and
the current statement of it is
[`web-product-process.md#shared-git-hooks`](../rules/web-product-process.md#shared-git-hooks) plus its
rationale. No new ADR was written for the reversal; the fact was absorbed into that rule.

**Overturns** [`web-product-process.md#shared-git-hooks`](../rules/web-product-process.md#shared-git-hooks)
on one clause: the shared thing is no longer a directory of hooks enumerated as *"pre-commit fmt,
pre-push clippy"*, but a single installed set whose membership is a directory listing. The rule is
marked pending absorption in the same change, along with the `## Now` prose that restates it.

## Context

Two hook directories were live in this repo at once, and `core.hooksPath` holds one value.

`CONTRIBUTING.md` said `git config core.hooksPath .githooks` and called it *"the only manual
step"*. `.githooks/` held the `cargo fmt` pre-commit and the `cargo clippy` pre-push. The rules
index said, separately, to run `scripts/install-hooks.sh` once per clone — and that script set
`core.hooksPath scripts/hooks`, where a different pre-commit ran the rules reference-linter and
regenerated the derived index.

Whichever instruction a contributor followed **last** silently disabled the other set, and the two
failure modes were not symmetric:

- Follow `CONTRIBUTING.md` and the rules index never regenerates locally, so drift reaches CI. The
  `--check` backstop turns that into a red build after a push — which is the exact feedback loop
  the pre-commit hook exists to shorten.
- Follow the rules index and the fmt/clippy pre-flight is gone entirely. That one has no backstop
  at the same stage at all; you find out at CI.

Neither document mentioned the other, and neither was wrong on its own terms. That is the shape of
the defect worth recording: **nothing was misconfigured.** Two correct instructions were written
against a single-valued setting, months apart, and the collision was invisible from inside either
one because a hook that never runs is indistinguishable from a hook that had nothing to say.

The obvious repair — merge the two directories by hand — fixes today's instance and rebuilds the
fork the next time anyone adds a check, because it leaves the same incentive in place: a new check
means editing a file that already belongs to someone else's check, or starting a directory of your
own.

## Decision

**`core.hooksPath` points at one directory forever, and that directory dispatches.**

`.githooks/pre-commit` and `.githooks/pre-push` are two-line stubs that `exec`
`.githooks/dispatch` with their own name. `dispatch` runs every executable under
`.githooks/<hook>.d/` in filename order, hands each one the hook's arguments and a verbatim replay
of the hook's stdin, and stops at the first non-zero exit. Adding a check is adding a file; it can
never displace one.

**`scripts/install-hooks.sh` is the single documented install command**, and it prints the
resulting roster. `CONTRIBUTING.md`, `AGENTS.md` and the rules index all now name that one command,
so the two instructions that collided have become the same instruction. The old
`git config core.hooksPath .githooks` line still does the right thing, because `.githooks/` is the
target that survived — a contributor working from a stale README is behind, not broken.

The four checks that exist today are the two directories' contents, unmerged and unchanged:
`10-rules-refs`, `20-rust-fmt`, `30-rules-index` under `pre-commit.d/`, and `10-rust-clippy` under
`pre-push.d/`.

**Ordering is a convention carried by the filename: read-only checks take the low numbers, a check
that writes takes a high one.** `30-rules-index` regenerates the derived index and re-stages it, so
a read-only check failing *after* it would abort the commit having left the author a modified,
staged file they never touched and were never told about. That constraint previously lived as a
paragraph inside the single file that happened to contain both checks; it is now expressible
between checks that do not know about each other, which is what made splitting them possible at
all.

**A check that is present but not executable is an error, not a skip.** The generalisation this
whole ADR turns on is that *a guard wired to something nothing executes reports nothing and
therefore reads as clean* — and a registry that silently passed over an unexecutable file would
reproduce that at a smaller scale, which is how the original defect would come back. So `dispatch`
refuses and names the file. There is deliberately **no** "disabled" marker to rename a check to:
retiring one is deleting it, where review sees it, and skipping one run is `--no-verify`. A first
draft reserved a `.disabled` suffix and it was cut for a reason worth recording — this repo's own
`check_rules_refs.py` requires every lane key in the tree to be classified, so the first person to
use the convention would have been met with an unclassified-lane failure from an unrelated guard.
The one name-based exclusion that survives is an editor's `~` backup, which inherits the executable
bit and would otherwise run as a stale duplicate of the check it shadows.

**`dispatch` names no toolchain, no check, and no path outside its own directory**, so it is the
same bytes in any repository; the stubs are two lines naming only their own hook, and
`install-hooks.sh` knows exactly one more thing — `.githooks` itself. Every other repo-specific fact
— a Rust toolchain, a Python guard, a docs tree — is inside a registry entry. That split is
deliberate and is the second reason for the shape: the portable half of a hook system is the
dispatch and the install, and the unportable half is every check anyone actually wants to run. A
shared installer that has to enumerate its checks is a shared installer that has to be edited per
repo.

**The first version of that claim was false, and the test written to guarantee it agreed anyway.**
`dispatch` carried a `see rules: web-product-process` comment and printed `./scripts/install-hooks.sh`
by hand — the first an anchor into this repo's rules corpus that a consumer's own linter would
reject, the second a path a consumer need not have. The test asked whether any of six words appeared
and neither of those is one of them. Both are gone; the message names the registry it was handed
instead; and the test now asserts the shape of the seam rather than sampling a blocklist. Recorded
because the failure is instructive: a guard whose subject is "does this text mention the wrong
thing" is only ever as good as the enumeration behind it, which is the same defect as a lane list
nobody adds to.

## Consequences

Both sets now fire from one install, which was the bug. The rules index regenerates locally again
for anyone who had followed `CONTRIBUTING.md`, and `cargo fmt`/`cargo clippy` run again for anyone
who had followed the rules index.

`scripts/hooks/` is gone. The CI path filter that watched it now watches `.githooks/**` and
`scripts/install-hooks.sh`. That filter re-runs the rules guards, which validate two of the four
checks and neither the dispatcher nor the installer — so the dispatcher gets its own suite,
`scripts/test_hook_dispatch.py`, wired into the same job. Some three hundred lines of shell now sit
in front of every commit in every clone, and this repo's habit is that a deterministic check ships
with the tests that hold it.

Two of the checks are Python, which makes `python3` a soft prerequisite it was not before on the
`CONTRIBUTING.md` path. They warn and step aside when it is absent rather than blocking every commit
in the clone: CI runs both regardless, and a public repo gets contributors who have a Rust toolchain
and nothing else. The warning is loud on purpose — a guard that goes quiet is the failure this ADR
is about, so the one thing that must never happen is skipping in silence.

The set is a hair slower to fail than one merged script would be, because a failing check aborts
before later checks report. Fail-fast is deliberate: a hook's job is to stop the commit, and a
contributor fixing two unrelated complaints at once is rarer than one being buried under the other.

This repo is public and a fork must work without reaching anything private, so the whole set stays
committed here, with no fetch, no submodule, and no plugin. **The portable half — `dispatch`, the two
stubs, `install-hooks.sh` — is POSIX `sh`; the registry entries are not**, and are not required to be:
`20-rust-fmt` and `10-rust-clippy` are `bash`, using `pipefail` and here-strings. That asymmetry is the
seam restated as a property. What sharing this across repos would mean is therefore a copy of the
portable half, not a dependency on it, and the unportable half is where a language choice is allowed
to follow the check.

#!/usr/bin/env python3
r"""ADR-number guard — a number is issued once, and a second decision may never carry it.

`docs/adr/README.md` states the rule and the damage: the `absorb-adrs` fold DELETES the ADR it
distils and leaves a `Distilled from: ADR-<n>` line in a rationale as the only surviving pointer to
it. That pointer is one-way. If two decisions ever carried the same number, the line cannot be
resolved to one of them by a human or a tool, ever — and it is unfixable after the fold, because the
file that would have disambiguated it is gone. So this is a guard rather than a convention: the
window in which the mistake is cheap closes the moment either ADR is absorbed.

Four checks, and each one exists because the obvious version of it is wrong.

  1. NO TWO FILES IN docs/adr/ SHARE A NUMBER. Exception-free, deliberately — it is the whole
     defect, it reads off the working tree with no history at all, and any allowlist here would be
     an allowlist on the live corpus rather than on the past.

  2. NO NUMBER IS LAST CARRIED BY TWO DISTINCT ADRs, across all history plus the working tree.
     "Last carried" is the load-bearing choice; see THE IDENTITY MODEL below.

  3. THE HISTORY IS USABLE. A shallow clone answers "which numbers were ever issued" with silence,
     and silence reads as green — the worst possible failure for a guard whose entire job is
     remembering. `actions/checkout` is depth 1 by default, so this is the DEFAULT state of a CI
     checkout, not an edge case. A shallow clone is a hard failure here, as is a live corpus that
     the visible history records no addition of. NOT detected, and said here rather than left to be
     found: a repository whose whole corpus arrived in ONE import commit is indistinguishable from a
     legitimate young one — every file is an addition, nothing was ever folded away, and the
     baseline it computes is the truth about that history. `actions/checkout` cannot produce that
     shape from this repo, so the gap is a statement about re-homing the corpus, not about CI.

  4. THE BASELINE IS REPORTED. The guard prints the highest number ever issued and the number the
     next ADR takes, so an author reads it off a tool instead of composing a `git log` incantation.
     That is not decoration; see THE ENUMERATION DEFECT.

THE ENUMERATION DEFECT this guard must not inherit. `docs/adr/README.md` used to prescribe

    git log --all --diff-filter=A --name-only --format="" -- 'docs/adr/*.md'

and that command is RENAME-BLIND. A renumber is a `git mv`, git reports it as `R`, and
`--diff-filter=A` excludes `R`. This repo has renumbered three times — 0044→0047 (2ba9f4e),
0047→0048 (795a572) and 0068→0074 (52e8a69) — and that command reports TWO numbers it has in fact
issued as never issued: 0048 and 0074. 0074 is live on disk right now and that command says nobody
ever took it. (0047 escapes only by accident: a different ADR had already been ADDED directly at
0047 before being moved off it, so the number appears in the additions for an unrelated reason. Two,
not three — the per-file blindness is total, and the number-level damage is what a near-miss
happened to reduce.) This guard therefore enumerates `--diff-filter=AR --name-status` with `-M` and
takes the DESTINATION of every rename, unioned with what is on disk today. The repair this guard
shipped alongside is itself a rename, so inheriting the defect would have made the guard forget the
very number it was written to protect.

MERGE COMMITS are the second half of that defect. `git log` emits NO diff for a merge commit by
default, so a rename performed INSIDE a merge — resolving a duplicate at the moment two branches
meet, which is precisely when a duplicate becomes visible — leaves no `R` record at all. The trap is
specific: the pre-commit fragment passes, because the rename is in `git diff HEAD`; then CI reds
forever, because CI checks out the merge and reads its history; and the repair is invisible to both.
`--diff-merges=first-parent` closes the direction that matters, which is merging the integration
branch INTO a topic branch — the workflow that produces the duplicate in the first place.

`separate` IS DISQUALIFIED, and this is worth the paragraph because it is the obvious upgrade and it
is a trap. `separate` diffs the merge against EVERY parent, which does catch both directions — but a
cross-parent diff pairs files that were never related, and git's rename detection scores those pairs.
Measured, on a fixture of ordinary ADR boilerplate (shared `## Status` / `## Context` / `## Decision`
/ `## Consequences` scaffolding) where a topic branch genuinely reuses 0002 while the integration
branch absorbed the old 0002 and added an unrelated 0005:

    R095    docs/adr/0002-old.md    docs/adr/0005-devnew.md      <- entirely spurious

That edge welds the absorbed 0002 onto the unrelated 0005, moves its terminal to 0005, and the REAL
reuse of 0002 disappears from the report — `separate` returns 0 problems where the default and
`first-parent` both name the collision. A silent false negative in the one thing this file exists to
do is worse than any false positive it prevents, and it is the same failure the rename-similarity
threshold is left alone to avoid (below). A guard fails loud or it is not a guard.

The residual gap `first-parent` leaves is real and is stated under WHAT THIS MODEL DOES NOT CATCH.
`--diff-merges` also emits some additions twice; `additions` feeds only `find()` (idempotent) and a
`max()` over a dict keyed by name, so that is a no-op.

THE IDENTITY MODEL, and why "last carried" rather than "ever carried".

An ADR is tracked through renames: rename records are union-find edges over `docs/adr/` basenames,
so a file that was renumbered is ONE identity that carried two numbers. An identity's TERMINAL path
is the one on disk today, or — for a deleted ADR — the last path it held before it was deleted
(operationally: the member of its component that was never the source of a rename). Check 2 asks
whether two identities share a TERMINAL number.

The alternative — "no number was EVER carried by two identities" — is the more obvious check and it
is the wrong one, on both correctness and cost:

  * It is wrong about what causes the damage. A `Distilled from:` line records the number the ADR
    carried WHEN IT WAS DELETED. A number an ADR held for twenty minutes before a renumber moved it
    can never appear in one. The terminal number is exactly the set of numbers that can.

  * It reds on every REPAIR. Resolving a collision means moving one file to a fresh number, which
    makes the ever-carried set of the vacated number permanently two. This repo has performed that
    repair three times (0044, 0047, 0068 above). Under "ever carried" all three, plus the collision
    this guard shipped with, need a permanent allowlist entry — four entries describing four bugs
    that were correctly fixed. Under "last carried" a repair CLEARS the finding, which is the
    behaviour you want from a guard: it goes green because the problem is gone.

WHAT THIS MODEL DOES NOT CATCH, stated plainly rather than discovered later.

  * A number VACATED BY A RENUMBER and left with no terminal holder, and a number BELOW THE
    HIGH-WATER MARK that was never issued to anyone. Both are deviations from the README's "take the
    next number after the highest ever issued" — but neither can produce an ambiguous provenance
    line, because there is no deleted ADR carrying that number for a `Distilled from:` line to name.
    The guard enforces the harm, not the convention; check 4 prints the baseline so the convention
    is easy to follow anyway. Neither case exists in this repo: every number the three renumbers
    vacated is held by the ADR the renumber was performed FOR, and 1..the high-water mark has no
    gaps.

  * A RENUMBER MADE INSIDE A MERGE WHOSE FIRST PARENT DOES NOT CARRY THE FILE. Merging a topic
    branch INTO the integration branch and renumbering the topic's ADR in that same commit puts the
    information only in the diff against the SECOND parent, which `first-parent` does not read (and
    `separate`, which would, is disqualified above). The guard then reds on a correctly repaired
    tree. It fails LOUD, which is the direction that is survivable, and it has an escape the silent
    case does not: DO THE RENUMBER AS ITS OWN COMMIT AFTER THE MERGE, never inside it. A test pins
    this behaviour deliberately, so switching modes to "fix" it cannot happen without reading why.

  * A RENUMBER GIT DOES NOT SCORE AS A RENAME. Rename detection is a similarity threshold (git's
    default, 50%), so renumbering a file and rewriting most of it in the same commit breaks the
    edge: the identity splits in two and the vacated number keeps a terminal holder that no longer
    exists. The threshold is left at the default ON PURPOSE. Lowering it trades a loud false
    positive for a SILENT false negative — ADRs share enough Markdown scaffolding that an aggressive
    threshold can score a deleted ADR and an unrelated new one as one rename, collapsing two
    identities into one and hiding exactly the collision this file exists to find. A guard fails
    loud or it is not a guard. What removes the hazard is procedure, not tuning: a renumber is a
    `git mv` plus the title line and nothing else, which is what `docs/adr/README.md` says and what
    52e8a69 did.

KNOWN_COLLISIONS covers the two genuine, unrepaired collisions this repo's history already contains.
Each entry names the exact set of terminal filenames that may share the number, so a THIRD file
landing on one still reds, and each carries its reason — the `check_sample_alias.py` allowlist
idiom. It is scoped to check 2 only. Check 1 has no exceptions and never will.

WHAT THE BASELINE DEPENDS ON — EVERY REF, and that is a deliberate trade with a bill attached.
Enumeration is `git log --all`, so the answer is as complete as the refs the checkout can see, and
that is far more than the current branch. `actions/checkout` at `fetch-depth: 0` fetches
`+refs/heads/*:refs/remotes/origin/*` (verified in this job's own log), so in CI `--all` reaches
EVERY branch in the repository, not just the base and the PR. Locally it also reaches branches that
were never pushed.

What that buys is the case the ticket was written about: two open pull requests, thirty-six minutes
apart, each taking "the next number after the highest", neither visible to the other. Both branches
are refs, so the guard sees both and reds — this DOES catch a collision between two open PRs, which
an enumeration scoped to `HEAD` could not. It is the no-false-negative direction, and that is why
`--all` is kept.

What it costs, said out loud: the verdict depends on refs that are not in the change under test. An
ABANDONED remote branch carrying a duplicate number reds this job for every PR that trips its path
filter, and the change that reds is innocent of the duplicate. The fix is to delete the stale
branch, which is a one-line remedy and leaves no residue — but a reader hitting a red here should
look for a branch as readily as for a file, so the report distinguishes files that are in this
working tree from those that are not rather than calling the latter "deleted". Scoping enumeration
to `HEAD` would remove the wedge and reintroduce the false negative; the trade was made in that
direction on purpose.

Stdlib only. Exit non-zero on any violation. Reads the WORKING TREE for the live corpus — so the
pre-commit fragment sees the ADR you just wrote — plus `git log --all` for committed history and
`git diff HEAD` for the rename you have staged but not yet committed. That last source is what lets
the repair PASS: renumbering is a `git mv`, and a guard that only read committed history would red
on the very commit that fixes the collision it reported.

Usage: python3 scripts/check_adr_numbers.py [root=.]
"""
from __future__ import annotations

import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

ADR_DIR = "docs/adr"

# An ADR file is a four-digit number, a hyphen, a slug. `README.md` and anything else in the
# directory is not an ADR and carries no number.
ADR_NAME_RE = re.compile(r"^(\d{4})-.+\.md$")

# The collisions this repo's history already contains and cannot un-contain. Each key is a number;
# each value is the exact set of TERMINAL filenames permitted to share it. A file outside the set
# landing on the same number is still a violation, and check 1 (two live files) is never exempt.
KNOWN_COLLISIONS: dict[int, frozenset[str]] = {
    # 0031 — one commit (1711bea, 2026-06-28) added the ADR together with two working documents
    # that borrowed its number instead of taking their own. The two plan docs were deleted in a
    # docs cleanup (6c371f7) and the ADR was absorbed later (dad634b). Neither plan doc was ever a
    # decision, so no rationale can carry a provenance line naming one, and there is nothing left
    # to renumber — every file involved is gone.
    31: frozenset({
        "0031-float-resolves-to-value-or-signal-by-wiring.md",  # the ADR
        "0031-impl-prep.md",                                    # a plan doc, not a decision
        "0031-tdd-plan.md",                                     # a plan doc, not a decision
    }),
    # 0035 — a retract-and-reissue from before the rule existed: the first ADR was added 2026-06-29
    # (1b98058) and deleted the next day as misguided (f258168), and a second ADR took the freed
    # number hours later (6f53414). It is a genuine reuse; both files are long gone, the second was
    # absorbed (dad634b), and the first left no rationale behind because it was never distilled.
    35: frozenset({
        "0035-typed-edit-time-constant.md",     # retracted the day after it landed
        "0035-constants-are-immutable-ports.md",  # took the freed number, later absorbed
    }),
}


def _git(root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(["git", "-C", str(root), *args],
                          capture_output=True, text=True)


def repository_problems(root: Path) -> list[str]:
    """Reasons the history under `root` cannot answer the question — each one fatal.

    A guard that reads history has exactly one catastrophic failure mode: a truncated history that
    reports nothing and is read as nothing-to-report. These are checked FIRST and short-circuit the
    run, so an unusable repository is loud rather than green.
    """
    probe = _git(root, "rev-parse", "--is-inside-work-tree")
    if probe.returncode != 0 or probe.stdout.strip() != "true":
        return [f"{root}: not a git work tree — this guard reads history and cannot "
                f"verify a number was never issued without it"]
    shallow = _git(root, "rev-parse", "--is-shallow-repository")
    if shallow.returncode != 0:
        return [f"{root}: git would not say whether this clone is shallow "
                f"({shallow.stderr.strip()}) — refusing to report on a history that may be cut off"]
    if shallow.stdout.strip() == "true":
        return [f"{root}: SHALLOW CLONE — the visible history is cut off, so every number ever "
                f"issued and then folded away is invisible and this guard would pass vacuously. "
                f"Fetch the full history (in Actions: `fetch-depth: 0` on the checkout step)"]
    return []


def _parse_name_status(text: str) -> tuple[list[tuple[str, str]], list[str]]:
    """`(renames, additions)` from `--name-status` output. Non-ADR filenames are dropped."""
    renames: list[tuple[str, str]] = []
    additions: list[str] = []
    for line in text.splitlines():
        if not line.strip():
            continue
        fields = line.split("\t")
        status = fields[0]
        if status.startswith("R") and len(fields) >= 3:
            src, dst = Path(fields[1]).name, Path(fields[2]).name
            if ADR_NAME_RE.match(src) and ADR_NAME_RE.match(dst):
                renames.append((src, dst))
            elif ADR_NAME_RE.match(dst):
                additions.append(dst)
        elif status.startswith("A") and len(fields) >= 2:
            name = Path(fields[1]).name
            if ADR_NAME_RE.match(name):
                additions.append(name)
    return renames, additions


def rename_records(root: Path) -> tuple[list[tuple[str, str]], list[str], list[str]]:
    """`(renames, additions, problems)` for every ADR path this repository has ever held.

    `renames` are `(source_basename, destination_basename)` pairs; `additions` are basenames. `-M`
    is passed explicitly so the answer does not depend on a clone's `diff.renames` setting.

    Two sources, and the second one is not an optimisation. Committed history is `git log --all`.
    UNCOMMITTED work is `git diff HEAD`, because the renumber this guard demands is a `git mv` that
    is staged and not yet committed at the moment the pre-commit fragment runs — without it the
    guard reds on the exact repair it just asked for, which is how a check gets bypassed instead of
    obeyed. Reading the pending diff keeps `live` (the working tree) and the rename edges that
    explain it in the same tense.

    `--diff-merges=first-parent` is load-bearing: without it `git log` emits no diff at all for a
    merge commit, and a renumber made while resolving a merge leaves no record. The module docstring
    argues why `first-parent` and NOT `separate` — `separate` scores renames across cross-parent
    diffs and invents edges that hide real collisions. It requires git 2.31 or newer; on anything
    older `git log` exits non-zero and this returns that as a problem, which is the loud failure the
    rest of this file is built around.
    """
    proc = _git(root, "log", "--all", "-M", "--diff-merges=first-parent", "--diff-filter=AR",
                "--name-status", "--format=", "--", ADR_DIR)
    if proc.returncode != 0:
        return [], [], [f"{root}: `git log` over {ADR_DIR} failed "
                        f"({proc.stderr.strip()}) — cannot enumerate the numbers ever issued"]
    renames, additions = _parse_name_status(proc.stdout)
    # No HEAD yet (an empty repository) means nothing is pending; the caller's "history records no
    # ADR" check is what speaks to that state, so a failure here is silence rather than noise.
    pending = _git(root, "diff", "-M", "--diff-filter=AR", "--name-status", "HEAD", "--", ADR_DIR)
    if pending.returncode == 0:
        more_renames, more_additions = _parse_name_status(pending.stdout)
        renames.extend(more_renames)
        additions.extend(more_additions)
    return renames, additions, []


def on_disk(root: Path) -> list[str]:
    """Every ADR filename currently in `<root>/docs/adr`, sorted."""
    directory = root / ADR_DIR
    if not directory.is_dir():
        return []
    return sorted(p.name for p in directory.iterdir()
                  if p.is_file() and ADR_NAME_RE.match(p.name))


def number_of(name: str) -> int:
    match = ADR_NAME_RE.match(name)
    if match is None:
        raise ValueError(f"not an ADR filename: {name}")
    return int(match.group(1))


def _label(n: int) -> str:
    """A number as it is written in a filename. Built rather than spelled: the reference-linter
    bans the literal decision-record token in code, and this file is code."""
    return f"{n:04d}"


def analyse(renames: list[tuple[str, str]], additions: list[str],
            live: list[str]) -> tuple[list[str], int | None]:
    """`(problems, highest_number_ever_issued)` for one corpus.

    Pure: takes the enumerated history and the live listing, touches neither git nor the disk. The
    guard's whole argument lives here, so the suite can exercise every case without building a
    repository for each one.
    """
    parent: dict[str, str] = {}

    def find(x: str) -> str:
        parent.setdefault(x, x)
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(a: str, b: str) -> None:
        ra, rb = find(a), find(b)
        if ra != rb:
            parent[ra] = rb

    for name in additions:
        find(name)
    for name in live:
        find(name)
    sources = set()
    for src, dst in renames:
        union(src, dst)
        sources.add(src)

    if not parent:
        return [], None

    members: dict[str, set[str]] = defaultdict(set)
    for name in parent:
        members[find(name)].add(name)

    live_set = set(live)
    # An identity's terminal path: what it is called now, or what it was called last. A component
    # with neither (a rename cycle, which no real history produces) falls back to all of its paths,
    # so an unreadable component over-reports rather than disappearing.
    terminals: dict[int, set[str]] = defaultdict(set)
    for root_name, group in members.items():
        here = sorted(group & live_set) or sorted(group - sources) or sorted(group)
        for name in here:
            terminals[number_of(name)].add(name)

    problems: list[str] = []

    # Check 1 — two files in the live corpus. No exceptions.
    by_number: dict[int, list[str]] = defaultdict(list)
    for name in live:
        by_number[number_of(name)].append(name)
    for n in sorted(by_number):
        if len(by_number[n]) > 1:
            problems.append(
                f"{ADR_DIR}: number {_label(n)} is carried by {len(by_number[n])} live files "
                f"({', '.join(sorted(by_number[n]))}) — a number is issued once, so one of them "
                f"takes a fresh number above the highest ever issued")

    # Check 2 — two decisions ending their life on the same number, which is what a `Distilled
    # from:` line cannot disambiguate.
    for n in sorted(terminals):
        holders = terminals[n]
        if len(holders) < 2:
            continue
        if holders <= KNOWN_COLLISIONS.get(n, frozenset()):
            continue
        # NOT "(deleted)". A file absent from this checkout may be alive on another branch, moved
        # out of the directory, or renamed in a merge this history cannot see — and telling someone
        # a file was deleted while `ls` on another ref shows it is how a guard gets read as broken.
        # The label states only what was observed: it is not in the tree in front of you.
        described = ", ".join(f"{name}{'' if name in live_set else ' (not in this working tree)'}"
                              for name in sorted(holders))
        problems.append(
            f"{ADR_DIR}: number {_label(n)} was last carried by {len(holders)} distinct ADRs "
            f"({described}) — a provenance line naming it cannot be resolved to one of them")

    # The high-water mark is every number ever CARRIED, not every number still held: a number a
    # renumber vacated was still issued, and handing it back out is the reuse this file is about.
    return problems, max(number_of(name) for name in parent)


def collect_problems(root_arg: str = ".") -> tuple[list[str], int | None]:
    """`(problems, highest_number_ever_issued)` for the repository at `root_arg`."""
    root = Path(root_arg).resolve()
    fatal = repository_problems(root)
    if fatal:
        return fatal, None
    renames, additions, problems = rename_records(root)
    if problems:
        return problems, None
    live = on_disk(root)
    if live and not renames and not additions:
        return [f"{root}: {ADR_DIR} holds {len(live)} ADRs and the visible history records none of "
                f"them being added — this history cannot say which numbers were ever issued"], None
    return analyse(renames, additions, live)


def main(root_arg: str = ".") -> int:
    problems, highest = collect_problems(root_arg)
    for p in problems:
        print(p, file=sys.stderr)
    baseline = ("no ADRs found" if highest is None else
                f"highest number ever issued {_label(highest)}, "
                f"the next ADR takes {_label(highest + 1)}")
    print(f"check_adr_numbers: {len(problems)} problem(s); {baseline}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:2]))

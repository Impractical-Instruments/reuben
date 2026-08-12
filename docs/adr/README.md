# ADRs — the live iteration surface

ADRs record architectural decisions **while they're still moving**. Write them normally during
iteration: one decision per file, the usual context / decision / consequences shape. This
directory persists.

Once a decision has solidified, its durable form is a **rule** (+ rationale) under
[`docs/rules/`](../rules/README.md), not an ADR. The `absorb-adrs` skill periodically:

1. distills solidified ADRs into the relevant topic's rules and rationale docs,
2. drops a `Distilled from: ADR-00xx` provenance line into each rationale, and
3. deletes the absorbed ADRs (git keeps the history).

A human runs `absorb-adrs` on a cadence — it is not automatic. Superseded and dead-end ADRs
are culled in the same pass; only the reasoning that still applies survives, in the rationale.

**Numbers are never reused.** Because the fold deletes files, the highest number *in this directory*
is not the highest number ever issued — a new ADR takes the next number after the highest **ever
issued**, not after the highest still on disk. A reused number collides with the `Distilled from:`
lines the absorbed ADR left behind, which are the only surviving pointers to it and cannot be
disambiguated after the fact.

**Ask the guard for the number; do not compose the query.**

```sh
python3 scripts/check_adr_numbers.py .
# check_adr_numbers: 0 problem(s); highest number ever issued 0083, the next ADR takes 0084
```

This paragraph used to prescribe `git log --all --diff-filter=A --name-only --format="" --
'docs/adr/*.md'`, and that command is **rename-blind**. A renumber is a `git mv`, git reports it as
`R`, and `--diff-filter=A` excludes renames — so of this repo's three renumbers it reports ADR-0048
and ADR-0074 as never issued, and ADR-0074 is live in this directory right now. (ADR-0047 escapes
only by accident: a different ADR had been added directly at 0047 before being moved off it.) It is
blind to a rename inside a **merge commit** too, where `git log` emits no diff at all — and that is
the case that bites, because resolving a merge is when a duplicate first becomes visible. Hence the
rule below.

[`scripts/check_adr_numbers.py`](../../scripts/check_adr_numbers.py) unions additions, rename
destinations (merge commits included), the rename you have staged but not yet committed, and what is
on disk. It refuses to answer from a shallow clone rather than passing vacuously on a history that
was cut off, and it reds when two decisions share a number, whether both are live or one was folded
away months ago. It runs in CI as the `adr-numbers` job and locally from
[`scripts/hooks/pre-commit.d/15-adr-numbers`](../../scripts/hooks/pre-commit.d/15-adr-numbers) —
which steps aside with a warning if the clone is shallow or the machine has no `python3`, so CI is
the authority. Its docstring argues the design and states what it deliberately does not police.

**A renumber is a `git mv` plus the title line, in its own commit, and nothing else.** `52e8a69` is
the shape to copy. Both halves of that are load-bearing:

- **Nothing else in the commit.** Git recognises a renumber by file similarity, so rewriting the ADR
  in the same commit can push it below the threshold — and a renumber git does not score as a rename
  is one the guard cannot follow.
- **Its own commit, never inside a merge.** A merge that pulls the integration branch into your
  branch is usually where the duplicate first appears, and fixing it there is the obvious move. Do
  not: the guard reads a merge against its first parent only, so a renumber of a file that arrived
  from the *other* side leaves no trace and the guard reds on a tree you have already repaired.
  Finish the merge, then renumber in the next commit.

## Overturning a live rule

Because the fold is periodic and the ADR is immediate, an ADR can leave a rule stating the old now
in the present tense for weeks. So **an ADR that overturns a rule marks that rule in the same
change**:

```md
<a id="whole-document-edit"></a>
### <the rule, untouched>

Superseded by: ADR-0066 (pending absorption)
```

`check_rules_links.py` fails if an ADR names a rule anchor (`<topic>.md#<slug>`, as a link or in
backticks) whose rule carries no marker — and fails again later if a marker names an ADR that has
been absorbed away, so the fold has to clear it. Naming the anchor is what triggers this: nothing
can tell overturning from citing, so an ADR that only wants context names the **topic**, the way
code does.

The marker covers the rule line and nothing else, so it is not the whole obligation: the same claim
is usually restated in the topic's `## Now` prose and its `## Terms` entry, and the `## Terms` entry
is republished into the index glossary by the derive. An ADR that overturns a rule strikes or hedges
those restatements in the same change too — the guard cannot see them, and left alone they state the
old now in the present tense with nothing to notice it.

**Do not** cite ADR numbers from code. Code points at topics: `// see rules: <topic>`. The only
surviving ADR mentions anywhere are (a) `Distilled from:` lines in rationale docs and (b) the
live ADRs here.

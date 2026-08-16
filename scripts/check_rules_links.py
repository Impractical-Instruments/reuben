#!/usr/bin/env python3
"""Structural-integrity guard for the rules-doc system. Runs against any repo carrying one.

The repo is the ROOT argument, so one copy of this file serves every repo that adopts the
system. Nothing here reads a repo name, a topic slug, or a count of topics.

The checks, in three groups.

**Rule -> rationale** (walks each top-level `docs/rules/<topic>.md`; README.md excluded,
`_templates/` and `rationale/` never scanned as topics):

  (a) the topic has >=1 rule section (an `<a id="slug"></a>` anchor + `### heading`);
  (b) a rule anchor carries at most one `[why](...)` link. Rationale is optional: a rule whose
      reasoning is contained in its own sentence generates no file, and only a second competing
      rationale is a defect.
  (c) every such link resolves to a file that exists, relative to docs/rules/.

**Corpus-wide** (walks every `.md` under docs/rules/ except `_templates/`) — the ladder is only
navigable if every rung resolves, and the rungs below the topic docs were previously unchecked:

  (d) every Markdown link resolves: the file exists, and an `#anchor` fragment names a real
      `<a id>` or heading in it. Rationale files link each other and link back up to their rule,
      and those links were unguarded — an absorption pass that deletes a rule anchor leaves them
      pointing at nothing while the build stays green.
  (e) rationale -> rule is injective: every file under `rationale/` is the [why] target of exactly
      one rule, and sits at `rationale/<topic>/` for a topic doc that exists. Catches the orphan a
      deleted rule leaves behind, and the file two rules quietly share.

**ADR -> rule** (walks the live ADRs under docs/adr/, README.md excluded):

  (f) a rule anchor an ADR names resolves — the topic doc exists and carries that anchor. An ADR
      is the one surface that may name a rule and be named in turn; the rules corpus never points
      back, so this is the only direction there is to check.

Links inside fenced blocks and inline code spans are not links — prose about Markdown (a rationale
quoting the `[`x`](crate::x)` markup a doc comment shipped) must not be read as one.

Green on an empty tree (no topic docs yet) — so it wires into CI from day one, unlike the
reference-linter. Exit non-zero with `path: message` lines on any violation; print a summary.
Stdlib only.

Usage: python3 check_rules_links.py [root=.]
"""
from __future__ import annotations
import re, sys
from pathlib import Path

# A rule is materialized as a raw-HTML anchor above its H3 heading; the slug is what survives
# rewording. This matches the anchor line, capturing the slug.
ANCHOR_RE = re.compile(r'<a\s+id="([^"]+)"\s*>\s*</a>')
# The single rationale link under each rule: `[why](rationale/<topic>/<slug>.md)`.
WHY_RE = re.compile(r"\[why\]\(([^)]+)\)")
# Any inline Markdown link. The leading `(?<!!)` drops image embeds; the trailing group eats an
# optional `"title"` so it does not land in the target.
LINK_RE = re.compile(r'(?<!!)\[[^\]]*\]\(\s*([^)\s]+)(?:\s+"[^"]*")?\s*\)')
FENCE_RE = re.compile(r"^\s*(?:```|~~~)")
# A backtick run and everything to its matching run — inline code, whatever is inside it.
INLINE_CODE_RE = re.compile(r"(`+)(?:(?!\1).)*\1", re.S)
HEADING_RE = re.compile(r"^\s*#{1,6}\s+(.*?)\s*#*\s*$")
# Check (f). A rule anchor as an ADR spells it: a link target or bare prose, either way
# `<topic>.md#<slug>`.
RULE_ANCHOR_RE = re.compile(r"\b([a-z0-9-]+)\.md#([a-z0-9-]+)")
ADR_ID_RE = re.compile(r"^(\d{4})-")
# Targets this guard has no way to resolve, and no business guessing at.
EXTERNAL_PREFIXES = ("http://", "https://", "mailto:", "ftp://")


def prose_lines(text: str) -> list[tuple[int, str]]:
    """The document with code removed: `(1-based lineno, line)` for every line outside a fenced
    block, with inline code spans blanked. Only what is left is prose that can carry a link."""
    out: list[tuple[int, str]] = []
    in_fence = False
    for i, line in enumerate(text.split("\n"), 1):
        if FENCE_RE.match(line):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        out.append((i, INLINE_CODE_RE.sub(lambda m: " " * len(m.group(0)), line)))
    return out


def unfenced_lines(text: str) -> list[tuple[int, str]]:
    """`(1-based lineno, line)` for lines outside fenced blocks, inline code left intact.

    Check (g) wants the opposite of the link checks. A link inside backticks is prose *about*
    Markdown, so `prose_lines` blanks code spans; but an ADR naming the rule it overturns writes
    that anchor in backticks at least as often as in a link, and blanking them would make the
    commonest spelling invisible to the one check that most needs to see it. A fenced block is
    still an example rather than a reference, so fences are dropped either way."""
    out, in_fence = [], False
    for i, line in enumerate(text.split("\n"), 1):
        if FENCE_RE.match(line):
            in_fence = not in_fence
            continue
        if not in_fence:
            out.append((i, line))
    return out


def heading_slug(text: str) -> str:
    """GitHub's heading -> fragment slug, enough of it: drop code ticks and punctuation, lowercase,
    spaces to hyphens."""
    s = re.sub(r"[`*_]", "", text).strip().lower()
    s = re.sub(r"[^\w\s-]", "", s)
    return re.sub(r"\s+", "-", s.strip())


def anchors_of(path: Path) -> set[str]:
    """Every fragment a link may target in this file: explicit `<a id>` slugs plus heading slugs."""
    text = path.read_text(encoding="utf-8", errors="ignore")
    found = set(ANCHOR_RE.findall(text))
    for _, line in prose_lines(text):
        m = HEADING_RE.match(line)
        if m:
            found.add(heading_slug(m.group(1)))
    return found


def rule_whys(path: Path) -> list[tuple[str, list[str]]]:
    """`(rule slug, its [why] targets)` for each rule in a topic doc, in document order.

    `## ` H2 headings bound a rule's span (e.g. the `## Terms` block after `## Rules`); an H3
    rule heading (`### `) is NOT a boundary. A rule runs from its anchor to the next anchor or
    the next H2, whichever comes first."""
    lines = path.read_text(encoding="utf-8", errors="ignore").split("\n")
    anchors = [(i, m.group(1)) for i, ln in enumerate(lines)
               for m in (ANCHOR_RE.search(ln),) if m]
    h2 = [i for i, ln in enumerate(lines) if ln.lstrip().startswith("## ")]
    out: list[tuple[str, list[str]]] = []
    for idx, (a_line, slug) in enumerate(anchors):
        next_anchor = anchors[idx + 1][0] if idx + 1 < len(anchors) else len(lines)
        next_h2 = next((h for h in h2 if h > a_line), len(lines))
        span = "\n".join(lines[a_line:min(next_anchor, next_h2)])
        out.append((slug, WHY_RE.findall(span)))
    return out


def check_topic(path: Path, rules: Path, rel: str) -> list[str]:
    """Checks (a)-(c): the list of `rel: message` problems for one topic doc."""
    problems: list[str] = []
    rules_found = rule_whys(path)
    if not rules_found:
        problems.append(f"{rel}: no rule sections (expected >=1 `<a id=\"…\"></a>` + `### …`)")
        return problems

    for slug, whys in rules_found:
        if len(whys) > 1:
            problems.append(
                f"{rel}: rule '{slug}' has {len(whys)} [why] links (expected at most 1)")
        for target in whys:
            if not (rules / target).exists():
                problems.append(
                    f"{rel}: rule '{slug}' [why] target '{target}' does not exist")
    return problems


def corpus_files(rules: Path) -> list[Path]:
    """Every Markdown file the corpus owns. `_templates/` holds deliberately unresolvable
    placeholders (`rationale/<topic>/<rule-slug>.md`) and is not corpus text."""
    return sorted(p for p in rules.rglob("*.md") if "_templates" not in p.parts)


def check_link_targets(root: Path, rules: Path) -> list[str]:
    """Check (d): every link in every corpus file resolves, fragment included."""
    problems: list[str] = []
    anchor_cache: dict[Path, set[str]] = {}
    for path in corpus_files(rules):
        rel = path.relative_to(root).as_posix()
        # A topic doc's `[why]` links belong to check (c), which reports them against the rule
        # slug that owns them — a strictly better message. Drop them here so one break is one
        # problem line.
        is_topic = path.parent == rules and path.name != "README.md"
        for lineno, line in prose_lines(path.read_text(encoding="utf-8", errors="ignore")):
            if is_topic:
                line = WHY_RE.sub("", line)
            for target in LINK_RE.findall(line):
                if target.startswith(EXTERNAL_PREFIXES):
                    continue
                filepart, _, fragment = target.partition("#")
                dest = path if not filepart else (path.parent / filepart)
                if not dest.exists():
                    problems.append(f"{rel}:{lineno}: link target '{target}' does not exist")
                    continue
                if not fragment or dest.suffix != ".md":
                    continue
                dest = dest.resolve()
                if dest not in anchor_cache:
                    anchor_cache[dest] = anchors_of(dest)
                if fragment not in anchor_cache[dest]:
                    problems.append(
                        f"{rel}:{lineno}: link '{target}' names no anchor in {filepart or 'itself'}")
    return problems


def check_rationale_bijection(root: Path, rules: Path) -> list[str]:
    """Check (e): every rationale file is linked by exactly one rule, and sits under a directory
    named for a real topic. The other direction is not required — a rule may carry no rationale."""
    problems: list[str] = []
    rationale_dir = rules / "rationale"
    topics = {p.stem for p in rules.glob("*.md") if p.name != "README.md"}

    # Keyed by RULE, not by [why] occurrence: a rule that repeats its link is check (b)'s report,
    # and must not read here as two rules sharing one rationale.
    referenced: dict[Path, set[str]] = {}
    for path in sorted(rules.glob("*.md")):
        if path.name == "README.md":
            continue
        for slug, whys in rule_whys(path):
            for target in set(whys):
                dest = rules / target
                if dest.exists():  # a missing target is check (c)'s report, not a double count
                    referenced.setdefault(dest.resolve(), set()).add(f"{path.stem}#{slug}")

    if not rationale_dir.is_dir():
        return problems
    for path in sorted(rationale_dir.rglob("*.md")):
        rel = path.relative_to(root).as_posix()
        citers = sorted(referenced.get(path.resolve(), set()))
        if not citers:
            problems.append(f"{rel}: orphan rationale — no rule links it")
        elif len(citers) > 1:
            problems.append(
                f"{rel}: linked by {len(citers)} rules ({', '.join(citers)}); expected exactly 1")
        topic = path.parent.name
        if topic not in topics:
            problems.append(f"{rel}: sits under 'rationale/{topic}/', which is not a topic doc")
    return problems


def rule_spans(path: Path) -> dict[str, str]:
    """`{rule slug: the rule's text}` for one topic doc, using the same span rule as `rule_whys`."""
    lines = path.read_text(encoding="utf-8", errors="ignore").split("\n")
    anchors = [(i, m.group(1)) for i, ln in enumerate(lines)
               for m in (ANCHOR_RE.search(ln),) if m]
    h2 = [i for i, ln in enumerate(lines) if ln.lstrip().startswith("## ")]
    out: dict[str, str] = {}
    for idx, (a_line, slug) in enumerate(anchors):
        next_anchor = anchors[idx + 1][0] if idx + 1 < len(anchors) else len(lines)
        next_h2 = next((h for h in h2 if h > a_line), len(lines))
        out[slug] = "\n".join(lines[a_line:min(next_anchor, next_h2)])
    return out


def live_adrs(root: Path) -> dict[str, Path]:
    """`{ADR-00xx: file}` for every live ADR. README.md is the surface's own prose, not an ADR."""
    adr_dir = root / "docs" / "adr"
    if not adr_dir.is_dir():
        return {}
    out: dict[str, Path] = {}
    for path in sorted(adr_dir.glob("*.md")):
        if path.name == "README.md":
            continue
        m = ADR_ID_RE.match(path.name)
        if m:
            out[f"ADR-{m.group(1)}"] = path
    return out


def check_adr_rule_anchors(root: Path, rules: Path) -> list[str]:
    """Check (f): a rule anchor an ADR names resolves.

    References run one way. Nothing committed names an issue, PR, ADR or commit, so a rule never
    points at the ADR that produced it and a rationale carries no provenance line. ADRs are the
    exception, being temporary by design: one may name the rule it overturns, and that pointer is
    how a pending absorption is found — grep the live ADRs for rule anchors.

    An unresolvable pointer is the defect this catches. A rule renamed or deleted after the ADR
    was written leaves the ADR aimed at nothing, and the absorption pass that would have noticed
    is the one that never runs because the pointer no longer resolves.
    """
    problems: list[str] = []
    spans = {p.stem: rule_spans(p) for p in rules.glob("*.md") if p.name != "README.md"} \
        if rules.is_dir() else {}

    for path in live_adrs(root).values():
        rel = path.relative_to(root).as_posix()
        seen: set[tuple[str, str]] = set()
        for lineno, line in unfenced_lines(path.read_text(encoding="utf-8", errors="ignore")):
            for topic, slug in RULE_ANCHOR_RE.findall(line):
                if (topic, slug) in seen:
                    continue        # an ADR names the rule it overturns more than once; one finding
                seen.add((topic, slug))
                if topic not in spans:
                    problems.append(f"{rel}:{lineno}: names rule '{slug}' in '{topic}.md', which is "
                                    f"not a topic doc")
                elif slug not in spans[topic]:
                    problems.append(f"{rel}:{lineno}: names '{topic}#{slug}', which is not a rule "
                                    f"anchor in that topic")
    return problems


def collect_problems(root_arg: str = ".") -> list[str]:
    root = Path(root_arg).resolve()
    rules = root / "docs" / "rules"
    problems: list[str] = []
    if rules.is_dir():
        for path in sorted(rules.glob("*.md")):  # top-level only; never _templates/ or rationale/
            if path.name == "README.md":
                continue
            rel = path.relative_to(root).as_posix()
            problems.extend(check_topic(path, rules, rel))
        problems.extend(check_link_targets(root, rules))
        problems.extend(check_rationale_bijection(root, rules))
    # Outside the `rules.is_dir()` guard: an ADR naming a rule anchor is a finding even in a tree
    # that has no corpus yet, and that is exactly when the topic doc is missing.
    problems.extend(check_adr_rule_anchors(root, rules))
    return problems


def main(root_arg: str = ".") -> int:
    problems = collect_problems(root_arg)
    for p in problems:
        print(p, file=sys.stderr)
    print(f"check_rules_links: {len(problems)} problem(s)", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:2]))

# ii:begin provenance — derived from .ii/repo.toml; do not hand-edit out of sync. Regenerate with `ii-generate --write .`. sha256=bcf2cb1fe93f7e33ad6bf2c12e2c19333086b2517401a3acbf1396ccebe57d01
# Source:   Impractical-Instruments/brain@440034f0365465428b89668736b6ae506e7c564c:machinery/rules/check_rules_links.py
# Fetched:  2026-08-16
# Refresh:  gh api 'repos/Impractical-Instruments/brain/contents/machinery/rules/check_rules_links.py?ref=main' --jq '.content' | base64 -d > scripts/check_rules_links.py && ii-generate --write .
# Do not edit locally. Changes go upstream via PR against the source repo.
# ii:end provenance

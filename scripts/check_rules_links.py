#!/usr/bin/env python3
"""Rule<->rationale link guard for the rules-doc system. Runs in both repos.

Walks each top-level `docs/rules/<topic>.md` (README.md excluded; `_templates/` and `rationale/`
are never scanned — only files directly under docs/rules/) and asserts the epic's core invariant:

  (a) the topic has >=1 rule section (an `<a id="slug"></a>` anchor + `### heading`);
  (b) every rule anchor carries exactly one `[why](...)` link;
  (c) every such link resolves to a file that exists, relative to docs/rules/.

Green on an empty tree (no topic docs yet) — so it wires into CI from day one, unlike the
reference-linter. Exit non-zero with `path: message` lines on any violation; print a summary.
Stdlib only.

Usage: python3 scripts/check_rules_links.py [root=.]
"""
from __future__ import annotations
import re, sys
from pathlib import Path

# A rule is materialized as a raw-HTML anchor above its H3 heading; the slug is what survives
# rewording. This matches the anchor line, capturing the slug.
ANCHOR_RE = re.compile(r'<a\s+id="([^"]+)"\s*>\s*</a>')
# The single rationale link under each rule: `[why](rationale/<topic>/<slug>.md)`.
WHY_RE = re.compile(r"\[why\]\(([^)]+)\)")


def check_topic(path: Path, rules: Path, rel: str) -> list[str]:
    """Return the list of `rel: message` problems for one topic doc."""
    problems: list[str] = []
    lines = path.read_text(encoding="utf-8", errors="ignore").split("\n")

    anchors = [(i, m.group(1)) for i, ln in enumerate(lines)
               for m in (ANCHOR_RE.search(ln),) if m]
    if not anchors:
        problems.append(f"{rel}: no rule sections (expected >=1 `<a id=\"…\"></a>` + `### …`)")
        return problems

    # `## ` H2 headings bound a rule's span (e.g. the `## Terms` block after `## Rules`); an H3
    # rule heading (`### `) is NOT a boundary. A rule runs from its anchor to the next anchor or
    # the next H2, whichever comes first.
    h2 = [i for i, ln in enumerate(lines) if ln.lstrip().startswith("## ")]
    for idx, (a_line, slug) in enumerate(anchors):
        next_anchor = anchors[idx + 1][0] if idx + 1 < len(anchors) else len(lines)
        next_h2 = next((h for h in h2 if h > a_line), len(lines))
        span = "\n".join(lines[a_line:min(next_anchor, next_h2)])
        whys = WHY_RE.findall(span)
        if len(whys) != 1:
            problems.append(
                f"{rel}: rule '{slug}' has {len(whys)} [why] link(s) (expected exactly 1)")
        for target in whys:
            if not (rules / target).exists():
                problems.append(
                    f"{rel}: rule '{slug}' [why] target '{target}' does not exist")
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
    return problems


def main(root_arg: str = ".") -> int:
    problems = collect_problems(root_arg)
    for p in problems:
        print(p, file=sys.stderr)
    print(f"check_rules_links: {len(problems)} problem(s)", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:2]))

#!/usr/bin/env python3
"""Derive guard + collator for the rules index. Runs in both repos.

docs/rules/README.md is DERIVED from the topic docs:
  - `## Topics`  collates each topic's `# title` + `> summary`;
  - `## Glossary` collates each topic's `## Terms` entries (linking the defining topic).

Modes:
  --check  (default) assert README's derived sections match the topics, both ways. CI backstop.
  --write            re-generate those sections in place from the topics. Used by the pre-commit
                     hook so drift is fixed locally and never reaches CI.

Deterministic ordering: topics by title, terms by term. Stdlib only.
Usage: python3 scripts/check_rules_derive.py [--check|--write] [root=.]
"""
from __future__ import annotations
import re, sys
from pathlib import Path

TERM_RE  = re.compile(r"-\s+\*\*(.+?)\*\*\s+—\s+(.+)")
TOPIC_RE = re.compile(r"-\s+\*\*\[(.+?)\]\((.+?\.md)\)\*\*\s+—\s+(.+)")
GLOSS_RE = re.compile(r"-\s+\*\*(.+?)\*\*\s+—\s+(.+?)\s+·\s+\[.+?\]\((.+?\.md)\)")


def parse_topic(path: Path):
    """Return (title, summary, {term: definition})."""
    title = summary = None
    terms, in_terms = {}, False
    for ln in path.read_text(encoding="utf-8", errors="ignore").splitlines():
        s = ln.strip()
        if title is None and s.startswith("# "):
            title = s[2:].strip()
        if summary is None and s.startswith(">"):
            summary = s[1:].strip()  # strip exactly one blockquote marker (keep any inner `>`)
        if s.startswith("## "):
            in_terms = s[3:].strip().lower() == "terms"
            continue
        if in_terms:
            m = TERM_RE.match(s)
            if m:
                terms[m.group(1).strip()] = m.group(2).strip()
    return title, summary, terms


def collect(rules: Path):
    """Gather (topics, terms, errors) from the topic docs. topics: file -> (title, summary)."""
    topics, terms, errors = {}, {}, []
    for path in sorted(rules.glob("*.md")):
        if path.name == "README.md":
            continue
        title, summary, tterms = parse_topic(path)
        if not title:
            errors.append(f"{path.name}: missing `# title` heading")
        if not summary:
            errors.append(f"{path.name}: missing `> summary` line")
        if title and summary:
            topics[path.name] = (title, summary)
        for t, d in tterms.items():
            if t in terms:
                errors.append(f"{path.name}: term '{t}' also defined in {terms[t][1]}")
            else:
                terms[t] = (d, path.name)
    return topics, terms, errors


def render_topics(topics):
    return [f"- **[{title}]({f})** — {summary}"
            for f, (title, summary) in sorted(topics.items(), key=lambda kv: kv[1][0].lower())]


def render_glossary(terms):
    return [f"- **{t}** — {d} · [{f[:-3]}]({f})"
            for t, (d, f) in sorted(terms.items(), key=lambda kv: kv[0].lower())]


def parse_readme(path: Path):
    topics, gloss, section = {}, {}, None
    for ln in path.read_text(encoding="utf-8", errors="ignore").splitlines():
        s = ln.strip()
        if s.startswith("## "):
            section = s[3:].strip().lower()
            continue
        if section == "topics":
            m = TOPIC_RE.match(s)
            if m:
                topics[m.group(2)] = (m.group(1).strip(), m.group(3).strip())
        elif section == "glossary":
            m = GLOSS_RE.match(s)
            if m:
                gloss[m.group(1).strip()] = (m.group(2).strip(), m.group(3))
    return topics, gloss


def splice(text: str, section: str, body: list[str]) -> str:
    """Replace the list body under `## <section>` with a CANONICAL block, dropping the old body up
    to the next `## ` heading or EOF. The block is: one blank line, the section's leading HTML
    comment line(s), the collated entries (possibly empty), one trailing blank.

    Idempotent — a second `--write` yields byte-identical output for empty, populated, and
    at-EOF sections. (The old code preserved leading blanks verbatim, so on an empty section it
    re-absorbed the prior run's trailing blank and then appended a fresh one, growing the file by a
    line each run; the entries were the only "wall" stopping that, so only the empty case drifted.)"""
    lines, out, i, n = text.split("\n"), [], 0, len(text.split("\n"))
    heading = f"## {section}".lower()
    while i < n:
        out.append(lines[i])
        if lines[i].strip().lower() == heading:
            i += 1
            # Preserve the leading HTML comment(s) verbatim — single- or multi-line — skipping any
            # blank lines OUTSIDE a comment. Stop at the first real entry or the next `## ` heading.
            comments = []
            while i < n and not lines[i].startswith("## "):
                s = lines[i].strip()
                if s.startswith("<!--"):
                    # Consume the whole comment through its closing `-->`, keeping every line
                    # (including blanks inside the comment) verbatim.
                    comments.append(lines[i])
                    while "-->" not in lines[i] and i + 1 < n:
                        i += 1
                        comments.append(lines[i])
                    i += 1
                elif s == "":
                    i += 1
                else:
                    break
            # Drop the old entries up to the next `## ` heading / EOF.
            while i < n and not lines[i].startswith("## "):
                i += 1
            out.append("")
            out.extend(comments)
            out.extend(body)
            out.append("")
            continue
        i += 1
    return "\n".join(out)


def main(argv: list[str]) -> int:
    mode = "--write" if "--write" in argv else "--check"
    rest = [a for a in argv if a not in ("--check", "--write")]
    root = Path(rest[0] if rest else ".").resolve()
    rules = root / "docs" / "rules"
    readme = rules / "README.md"
    if not readme.exists():
        print("check_rules_derive: no docs/rules/README.md — nothing to do", file=sys.stderr)
        return 0

    topics, terms, errors = collect(rules)

    if mode == "--write":
        if errors:  # structural problems the collator can't paper over
            for e in errors:
                print(e, file=sys.stderr)
            return 1
        text = readme.read_text(encoding="utf-8")
        text = splice(text, "Topics", render_topics(topics))
        text = splice(text, "Glossary", render_glossary(terms))
        readme.write_text(text, encoding="utf-8")
        return 0

    # --check
    r_topics, r_gloss = parse_readme(readme)
    for f, (title, summary) in topics.items():
        if f not in r_topics:
            errors.append(f"README Topics missing '{f}'")
        elif r_topics[f] != (title, summary):
            errors.append(f"README Topics entry for '{f}' drifted — run check_rules_derive.py --write")
    for f in r_topics:
        if f not in topics:
            errors.append(f"README Topics lists '{f}' with no such topic doc")
    for t, (d, f) in terms.items():
        if t not in r_gloss:
            errors.append(f"README Glossary missing term '{t}' (from {f})")
        elif r_gloss[t] != (d, f):
            errors.append(f"README Glossary entry for '{t}' drifted — run check_rules_derive.py --write")
    for t in r_gloss:
        if t not in terms:
            errors.append(f"README Glossary lists term '{t}' defined in no topic")

    for e in errors:
        print(e, file=sys.stderr)
    print(f"check_rules_derive: {len(errors)} problem(s)", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

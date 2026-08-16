#!/usr/bin/env python3
"""Derive guard + collator for the rules index. Runs against any repo carrying one.

The repo is the ROOT argument, so one copy of this file serves every repo that adopts the
system — reached through a submodule, or vendored in and declared beside a provenance block.
Nothing here reads a repo name, a topic slug, or a count of topics.

docs/rules/README.md is DERIVED from the topic docs:
  - `## Topics`  collates each topic's `# title` + `> summary`;
  - `## Glossary` collates each topic's `## Terms` entries (linking the defining topic).

Modes:
  --check  (default) assert README's derived sections match the topics, both ways. CI backstop.
  --write            re-generate those sections in place from the topics. Used by the pre-commit
                     hook so drift is fixed locally and never reaches CI.

Deterministic ordering: topics by title, terms by term. Stdlib only.
Usage: python3 check_rules_derive.py [--check|--write] [root=.]
"""
from __future__ import annotations
import re, sys
from pathlib import Path

FENCE_RE = re.compile(r"^(?P<f>`{3,}|~{3,})(?P<info>.*)$")
TERM_RE  = re.compile(r"-\s+\*\*(.+?)\*\*\s+—\s+(.+)")
TOPIC_RE = re.compile(r"-\s+\*\*\[(.+?)\]\((.+?\.md)\)\*\*\s+—\s+(.+)")
GLOSS_RE = re.compile(r"-\s+\*\*(.+?)\*\*\s+—\s+(.+?)\s+·\s+\[.+?\]\((.+?\.md)\)")


def fenced(lines: list[str]) -> list[bool]:
    """Per line: is it inside a fenced code block, the fence lines themselves included.

    A `## ` inside a fence is an EXAMPLE of a heading, not one, and an index that documents its own
    format carries exactly that. Nothing here used to know the difference, and the cost was not a
    missed match but DATA LOSS: `splice` took a fenced `## Glossary` for the real section, rewrote
    from there to the next real heading, and ate the closing fence and every line after it — after
    which both `--check` and `--write` called the wreckage clean. `ii_generate.py` masks fences for
    this reason; this is the same rule in the file that ships to public CI.
    """
    mask, opener = [], None
    for ln in lines:
        m = FENCE_RE.match(ln.strip())
        if opener is None:
            inside = bool(m)
            if m:
                opener = m.group("f")
        else:
            inside = True
            # A closer is the same character, at least as long, and carries no info string.
            if (m and m.group("f")[0] == opener[0] and len(m.group("f")) >= len(opener)
                    and not m.group("info").strip()):
                opener = None
        mask.append(inside)
    return mask


def parse_topic(path: Path):
    """Return (title, summary, {term: definition})."""
    title = summary = None
    terms, in_terms = {}, False
    lines = path.read_text(encoding="utf-8", errors="ignore").splitlines()
    for ln, in_fence in zip(lines, fenced(lines)):
        if in_fence:
            continue
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
    lines = path.read_text(encoding="utf-8", errors="ignore").splitlines()
    for ln, in_fence in zip(lines, fenced(lines)):
        if in_fence:
            continue
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


def splice(text: str, section: str, body: list[str]) -> tuple[str, bool]:
    """Replace the list body under `## <section>` with a CANONICAL block, dropping the old body up
    to the next `## ` heading or EOF. The block is: one blank line, the section's leading HTML
    comment line(s), the collated entries (possibly empty), one trailing blank.

    Returns the new text and whether the heading was there at all. An absent heading is not an
    error here — an index that defines no terms legitimately carries no `## Glossary` — but it IS
    the difference between "wrote the entries" and "had nowhere to write them", and only the
    caller knows whether there were entries. Reporting it is what stops `--write` exiting 0 on a
    tree it did not converge.

    Idempotent — a second `--write` yields byte-identical output for empty, populated, and
    at-EOF sections. (The old code preserved leading blanks verbatim, so on an empty section it
    re-absorbed the prior run's trailing blank and then appended a fresh one, growing the file by a
    line each run; the entries were the only "wall" stopping that, so only the empty case drifted.)"""
    lines = text.split("\n")
    out, i, n = [], 0, len(lines)
    mask = fenced(lines)
    # A `## ` line only counts as a heading — to match on, or to stop at — when it is not inside a
    # fence. Both boundary scans below consult the same mask, so an example block is opaque to all
    # three rather than to whichever one happened to be fixed.
    def is_heading(j: int) -> bool:
        return lines[j].startswith("## ") and not mask[j]

    heading = f"## {section}".lower()
    found = False
    while i < n:
        out.append(lines[i])
        if not mask[i] and lines[i].strip().lower() == heading:
            found = True
            i += 1
            # Preserve the leading HTML comment(s) verbatim — single- or multi-line — skipping any
            # blank lines OUTSIDE a comment. Stop at the first real entry or the next `## ` heading.
            comments = []
            while i < n and not is_heading(i):
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
            while i < n and not is_heading(i):
                i += 1
            out.append("")
            out.extend(comments)
            out.extend(body)
            out.append("")
            continue
        i += 1
    return "\n".join(out), found


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
        # A section the index does not carry is a section `splice` cannot write into. Silently
        # writing nothing and exiting 0 leaves `--check` red with no way to satisfy it, and the
        # front-line fixer reporting success is worse than the drift: the pre-commit hook passes,
        # CI reds, and the per-term message names no remedy. An index with nothing to collate
        # legitimately omits the heading, so the refusal turns on there being entries.
        homeless = []
        for section, body in (("Topics", render_topics(topics)), ("Glossary", render_glossary(terms))):
            text, found = splice(text, section, body)
            if body and not found:
                homeless.append(section)
        if homeless:
            for section in homeless:
                print(f"{readme} has no `## {section}` section, and there is collated content for "
                      f"it. Add the heading, with a `<!-- derived — … -->` comment beneath it that "
                      f"says the body is collated and not to be hand-edited; or remove what feeds "
                      f"it. Nothing was written, including the sections that were fine.",
                      file=sys.stderr)
            return 1
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

# ii:begin provenance — derived from .ii/repo.toml; do not hand-edit out of sync. Regenerate with `python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .`. sha256=422de29065f2bde169f00ec759e940e273bff3f41ab8e1d7b8a4b7f098a1a6b1
# Source:   Impractical-Instruments/agent-tools@057c3f7a9391816263b1a4fcb46af5f4a5dc705f:plugins/impractical-doctrine/rules/check_rules_derive.py
# Fetched:  2026-08-12
# Refresh:  gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/rules/check_rules_derive.py?ref=main' --jq '.content' | base64 -d > scripts/check_rules_derive.py && ii-generate --write .
# Do not edit locally. Changes go upstream via PR against the source repo.
# ii:end provenance

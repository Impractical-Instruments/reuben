#!/usr/bin/env python3
"""Reference-linter for the rules-doc system. Runs in both engine and web repos.

Five checks:
  1. No `ADR-<n>` references survive in CODE. The only legitimate ADR mentions are
     `Distilled from:` lines in docs/rules/rationale/** and the live ADRs in docs/adr/**
     (both are Markdown, which this linter does not scan as code), plus the `absorb-adrs`
     skill itself — the tool that distils ADRs into rules and stamps each rationale's
     provenance line, so its scaffolder + tests name ADRs by design (see SKILL_ALLOWLIST).
  2. Every `see rules: <topic>` / `see engine rules: <topic>` code comment names a kebab-case
     slug; for the same-repo form, docs/rules/<topic>.md must exist. For the cross-repo form,
     the topic is resolved against the pinned engine submodule's engine/docs/rules/<topic>.md
     (the SHA web is built against) — a no-op in the engine repo, active once web bumps the pin.
  3. A `//!` module doc longer than MAX_UNPOINTED_MODULE_DOC lines names a topic. Length is a
     proxy for carrying rationale: a doc that long is arguing something, and an argument in code
     has to point at the rule it belongs to.
  4. No `//` or `//!` comment cites an issue (`#123`, `reuben#123`) or a rule anchor
     (`agent-mcp.md#some-rule`). An issue number is provenance, and provenance lives in a rationale
     file's `Decided in:` / `Distilled from:` line — the same reason check 1 bans `ADR-<n>`. In code
     it is an unresolvable pointer to a closed argument, and it reliably marks a comment that is
     retelling history rather than stating mechanics. A rule anchor is the deeper half of the same
     mistake: code points at topics only.

     `///` is deliberately out of reach here: this linter cannot tell a comment from an advertised
     description textually, and the two are governed differently — see rules: code-as-grounding.
     The wire half is guarded from the door's own output, in reuben-mcp's stdio integration test.
  5. A `see rules: <topic>` pointer stops at the topic. Check 4's `RULE_ANCHOR_RE` only sees the
     `<file>.md#<rule>` spelling; prose reaches a rule three other ways — `<topic>#<rule>`,
     a parenthesised `(<rule>, <rule>)` trailing the topic, and a comma-continued list — and
     `///` is out of check 4's reach entirely. Check 5 reads the pointer's own tail instead, so
     every spelling resolves or is reported. It also rejects a capitalised `see`, which the
     grammar does not admit and which therefore slips past checks 2 and 3 unvalidated.

     Both this and check 2 run over the comment-folded view, so a pointer rustfmt wrapped across
     a line break is still read whole.
     see rules: code-as-grounding

Exit non-zero on any violation. Stdlib only. Wired into CI in both repos (S19, epic #165) now
that the code is clean.

Usage: python3 scripts/check_rules_refs.py [root=.]
"""
from __future__ import annotations
import re, sys
from pathlib import Path

CODE_EXTS = {".rs", ".py", ".mjs", ".js", ".ts", ".jsx", ".tsx", ".go", ".c", ".h",
             ".cpp", ".hpp", ".java", ".rb", ".sh", ".toml", ".yml", ".yaml"}
SKIP_DIRS = {".git", "target", "node_modules", "dist", "build", "engine"}
# The absorb-adrs skill is the one sanctioned home of ADR-NNNN tokens in code: it distils ADRs
# into rules and writes each rationale's `Distilled from:` line, so its scaffolder + tests name
# ADRs by design. Exempt the skill directory (lives in the engine repo; harmless where absent).
SKILL_ALLOWLIST = {"absorb-adrs"}

ADR_RE  = re.compile(r"\bADR-\d+\b")
SEE_RE  = re.compile(r"\bsee (engine )?rules: ([A-Za-z0-9-]+)")
SLUG_RE = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
# The same pointer with the `see` unanchored to case. Only the lowercase spelling is grammar
# (docs/rules/README.md, Conventions); this exists so the other spelling is *reported* rather than
# skipped by SEE_RE, since an unmatched pointer is an unvalidated topic, not a clean file.
SEE_ANYCASE_RE = re.compile(r"\bsee (engine )?rules:[ \t]*", re.IGNORECASE)

# The unpointed `//!` budget. Ten lines is room for what a module doc legitimately owes a reader —
# what this file is, and the local facts the code cannot state — before the length itself says
# rationale accumulated.
MAX_UNPOINTED_MODULE_DOC = 10

# An issue citation in a comment: `#123`, or the cross-repo `reuben#123`. Two digits minimum, so
# `#3` in prose and a commented-out `#[derive(…)]` cannot trip it.
ISSUE_RE = re.compile(r"(?<![\w#])(?:[a-z][\w.-]*)?#\d{2,4}\b")
# A rule-level pointer: `agent-mcp.md#expect-guard-is-a-door-concern`. Code points at TOPICS only
# (docs/rules/README.md, Conventions) — a rule slug is the deepest rung and reworded freely, so a
# comment naming one is a link that breaks silently.
RULE_ANCHOR_RE = re.compile(r"\b[a-z0-9-]+\.md#[a-z0-9-]+")

# Check 5's three ways a pointer reaches past its topic, matched against the text right after it:
# `<topic>#<rule>`, a parenthesised list, and a comma-continued list. Each captured token is
# resolved as a topic; anything that is not one is a rule slug, the deepest rung, which code does
# not name. Nothing further out counts — ordinary prose follows a pointer all the time.
POINTER_ANCHOR_RE = re.compile(r"^#([a-z0-9-]+)")
POINTER_PARENS_RE = re.compile(r"^[\s.:—-]*\(\s*([a-z0-9-]+(?:\s*,\s*[a-z0-9-]+)*)\s*[,)]")
POINTER_COMMAS_RE = re.compile(r"^((?:\s*,\s*[a-z0-9-]+)+)")
# Which token opens a comment, by extension. Keyed off the suffix rather than sniffed generically:
# `#` opens a comment in the shell/Python half and is an attribute (`#[…]`) or a raw-string delimiter
# (`r#"…"#`) in Rust, and guessing wrong turns code into prose.
HASH_EXTS = {".py", ".sh", ".rb", ".toml", ".yml", ".yaml"}
# A continuation line's opener, stripped so a comment reads as one string across the line break
# rustfmt (or a human) put in it. `*` and `--` cover a `/* … */` block and SQL-ish comments.
CONT_MARKER_RE = re.compile(r"^(///|//!|//|#|\*|--)[ \t]?")


def comment_body(line: str, opener: str = "//", reach_docs: bool = True) -> tuple[int, str] | None:
    """(index just past the opener, body) of the line's comment, or None if it has none.

    A hand-rolled scan rather than a regex because the opener has to be outside string literals:
    the `//` in `"https://…"` is data, and a regex cannot see the quote that precedes it. With
    `reach_docs` false, a `///` doc comment returns None — check 4 deliberately does not reach one.
    """
    # Single quotes count as a literal only where `#` opens comments: in Rust a lone `'` is a
    # lifetime, and treating it as an open string would swallow the rest of the line.
    quotes = "\"'" if opener == "#" else '"'
    i, n, in_str = 0, len(line), ""
    while i < n:
        c = line[i]
        if in_str:
            if c == "\\":
                i += 2
                continue
            if c == in_str:
                in_str = ""
        elif c in quotes:
            in_str = c
        elif line.startswith(opener, i):
            j = i + len(opener)
            if opener == "//":
                if line.startswith("///", i) and not reach_docs:
                    return None
                while j < n and line[j] in "/!":      # the `/` of `///`, the `!` of `//!`
                    j += 1
            return j, line[j:]
        i += 1
    return None


def fold_comments(text: str, opener: str = "//") -> tuple[str, list[int]]:
    """The file's comment prose alone, line breaks inside one comment folded to a space.

    Two things a line-by-line scan gets wrong, fixed in one pass. A pointer is a sentence and gets
    broken wherever the column runs out — `see rules:` on one line, its topic on the next — so
    reading a line sees a fragment and matches nothing, which reads as a clean file. And a pointer
    in a string literal (this guard's own test fixtures) is data, not prose, so code is dropped
    rather than scanned. The returned list maps each folded offset back to its source offset, so a
    problem still names the line the reader has to go edit.
    """
    out: list[str] = []
    idx: list[int] = []
    pos = 0
    open_comment = False
    for raw in text.splitlines(True):
        body = raw.rstrip("\n")
        stripped = body.lstrip()
        lead = len(body) - len(stripped)
        cont = CONT_MARKER_RE.match(stripped) if open_comment else None
        if cont:
            start, prose = lead + cont.end(), " "
        else:
            found = comment_body(body, opener)
            if not found:
                open_comment = False
                continue
            start, prose = found[0], "\n"
        if out:
            out.append(prose)
            idx.append(pos + start)
        for k in range(start, len(body)):
            out.append(body[k])
            idx.append(pos + k)
        open_comment = True
        pos += len(raw)
    return "".join(out), idx


def module_doc_problems(rel: str, text: str) -> list[str]:
    """Check 3 for one Rust file: the leading `//!` block is short, or it names a topic.

    The block is the first run of `//!` lines and ends at the first line that is not one. Reaching
    it means stepping over everything that legitimately precedes a module doc — blank lines, inner
    attributes (`#![…]`, which wrap across lines), a licence header in `//` or `/* … */`. Each of
    those is a way to put the doc out of the scan's reach, so each is skipped rather than treated
    as the first line of code.
    """
    doc: list[str] = []
    in_block = False                   # inside a /* … */
    attr_depth = 0                     # inside an inner attribute wrapped across lines
    for line in text.splitlines():
        s = line.strip()
        if in_block:
            in_block = "*/" not in s
            continue
        if attr_depth:
            attr_depth += s.count("[") - s.count("]")
            continue
        if s.startswith("//!"):
            doc.append(s)
            continue
        if doc:
            break                      # the block ended: first line of real code
        if not s or s.startswith("//"):
            continue                   # blank, or a plain comment sitting above the module doc
        if s.startswith("/*"):
            in_block = "*/" not in s[2:]
            continue
        if s.startswith("#!"):
            attr_depth = s.count("[") - s.count("]")
            continue
        break                          # code before any module doc: there is none
    if len(doc) <= MAX_UNPOINTED_MODULE_DOC or SEE_RE.search("\n".join(doc)):
        return []
    return [f"{rel}:1: {len(doc)}-line `//!` module doc names no topic — point the rationale at "
            f"one (`//! see rules: <topic>`) or cut it to mechanics "
            f"(<={MAX_UNPOINTED_MODULE_DOC} lines)"]


def comment_ref_problems(rel: str, text: str) -> list[str]:
    """Check 4 for one Rust file: a `//` or `//!` comment cites no issue and no rule anchor.

    Reads the comment body only, so an issue number inside a string literal (a test fixture, a
    URL) is not a violation — the target is prose that retells history, not data.
    """
    problems = []
    for i, line in enumerate(text.splitlines(), 1):
        found = comment_body(line, reach_docs=False)
        if not found:
            continue
        body = found[1]
        for ref in ISSUE_RE.findall(body):
            problems.append(f"{rel}:{i}: issue citation `{ref}` in a comment — provenance belongs "
                            f"in a rationale file, not in code; point at a topic instead")
        for ref in RULE_ANCHOR_RE.findall(body):
            problems.append(f"{rel}:{i}: rule-level pointer `{ref}` in a comment — point at the "
                            f"topic instead (`see rules: <topic>`)")
    return problems


def pointer_problems(rel: str, text: str, is_topic, opener: str = "//") -> list[str]:
    """Checks 2 and 5 for one file: every `see rules:` pointer resolves, and stops at its topic.

    Both run over the comment-folded view (`fold_comments`), so a pointer wrapped across a line
    break is read whole rather than missed. `is_topic(cross, slug)` answers whether a slug
    resolves to a topic doc; the two checks share it so they agree on what a topic is.
    """
    problems = []
    folded, idx = fold_comments(text, opener)
    line_starts = [0] + [i + 1 for i, c in enumerate(text) if c == "\n"]

    def line_at(pos: int) -> int:
        """The 1-based source line holding folded offset `pos`."""
        src = idx[pos] if pos < len(idx) else len(text)
        lo, hi = 0, len(line_starts) - 1
        while lo < hi:
            mid = (lo + hi + 1) // 2
            lo, hi = (mid, hi) if line_starts[mid] <= src else (lo, mid - 1)
        return lo + 1

    for m in SEE_ANYCASE_RE.finditer(folded):
        i = line_at(m.start())
        if folded[m.start():m.start() + 3] != "see":
            problems.append(f"{rel}:{i}: capitalised `{folded[m.start():m.end()].strip()}` — the "
                            f"grammar is lowercase `see rules: <topic>`, and only that spelling is "
                            f"validated")
            continue
        pointer = SEE_RE.match(folded, m.start())
        if not pointer:
            continue                   # prose *about* the grammar, e.g. this file's own docstring
        cross, topic = pointer.group(1), pointer.group(2)
        if not SLUG_RE.match(topic):
            problems.append(f"{rel}:{i}: malformed rules slug '{topic}' (kebab-case expected)")
            continue
        if not is_topic(cross, topic):
            where = "engine/docs/rules" if cross else "docs/rules"
            problems.append(f"{rel}:{i}: `see {'engine ' if cross else ''}rules: {topic}` has no "
                            f"{where}/{topic}.md")
        tail = folded[pointer.end():pointer.end() + 160]
        named: list[str] = []
        if anchor := POINTER_ANCHOR_RE.match(tail):
            named = [anchor.group(1)]
        elif parens := POINTER_PARENS_RE.match(tail):
            named = [s.strip() for s in parens.group(1).split(",")]
        elif commas := POINTER_COMMAS_RE.match(tail):
            named = [s.strip() for s in commas.group(1).split(",") if s.strip()]
        for slug in named:
            if not is_topic(cross, slug):
                problems.append(f"{rel}:{i}: `see rules: {topic}` reaches past its topic to "
                                f"`{slug}` — code points at topics only, so a rule can be reworded "
                                f"without silently breaking the pointer")
    return problems


def main(root_arg: str = ".") -> int:
    root = Path(root_arg).resolve()
    errors: list[str] = []

    def topic_path(cross, slug: str) -> Path:
        """Where a topic doc lives: in-repo, or under the pinned engine submodule for the
        cross-repo form (the SHA web is built against)."""
        base = root / "engine" / "docs" / "rules" if cross else root / "docs" / "rules"
        return base / f"{slug}.md"

    def is_topic(cross, slug: str) -> bool:
        return bool(SLUG_RE.match(slug)) and topic_path(cross, slug).exists()

    for path in root.rglob("*"):
        if not path.is_file() or path.suffix not in CODE_EXTS:
            continue
        parts = set(path.relative_to(root).parts)
        if parts & SKIP_DIRS or parts & SKILL_ALLOWLIST:
            continue
        rel = path.relative_to(root).as_posix()
        try:
            text = path.read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        if path.suffix == ".rs":
            errors.extend(module_doc_problems(rel, text))
            errors.extend(comment_ref_problems(rel, text))
        errors.extend(pointer_problems(rel, text, is_topic,
                                       "#" if path.suffix in HASH_EXTS else "//"))
        for i, line in enumerate(text.splitlines(), 1):
            if ADR_RE.search(line):
                errors.append(f"{rel}:{i}: ADR reference in code — point at a topic: `see rules: <topic>`")
    for e in errors:
        print(e, file=sys.stderr)
    print(f"check_rules_refs: {len(errors)} problem(s)", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:2]))

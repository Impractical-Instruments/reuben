#!/usr/bin/env python3
"""Reference-linter for the rules-doc system. Runs in both engine and web repos.

Four checks:
  1. No `ADR-<n>` references survive in CODE. The only legitimate ADR mentions are
     `Distilled from:` lines in docs/rules/rationale/** and the live ADRs in docs/adr/**
     (both are Markdown, which this linter does not scan as code), plus the `absorb-adrs`
     skill itself — the tool that distils ADRs into rules and stamps each rationale's
     provenance line, so its scaffolder + tests name ADRs by design (see SKILL_ALLOWLIST).
  2. Every `see rules: <topic>` / `see engine rules: <topic>` code comment names a kebab-case
     slug; for the same-repo form, docs/rules/<topic>.md must exist. For the cross-repo form,
     the topic is resolved against the pinned engine submodule's engine/docs/rules/<topic>.md
     (the SHA web is built against) — a no-op in the engine repo, active once web bumps the pin.
  3. In a SWEPT_CRATES crate, a `//!` module doc longer than MAX_UNPOINTED_MODULE_DOC lines
     names a topic. Length is a proxy for carrying rationale: a doc that long is arguing
     something, and an argument in code has to point at the rule it belongs to.
  4. In a SWEPT_CRATES crate, no `//` or `//!` comment cites an issue (`#123`, `reuben#123`) or a
     rule anchor (`agent-mcp.md#some-rule`). An issue number is provenance, and provenance lives in
     a rationale file's `Decided in:` / `Distilled from:` line — the same reason check 1 bans
     `ADR-<n>`. In code it is an unresolvable pointer to a closed argument, and it reliably marks a
     comment that is retelling history rather than stating mechanics. A rule anchor is the deeper
     half of the same mistake: code points at topics only.

     `///` is deliberately out of reach here: a FIELD doc on a `JsonSchema`-deriving type becomes
     that field's advertised schema `description` — model-facing wire surface, not comment prose —
     and this linter cannot tell the two apart textually. (A struct-level doc on a tool's `…Params`
     is not: rmcp advertises the `#[tool(description = …)]` string instead. Verify either way by
     diffing `tools/list` before and after.) Sweep those by hand, gated by `eval/`.
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

# Crates whose comments have been swept to the rationale/mechanics/restatement split.
# Checks 3 and 4 run only here, so the gate is a ratchet: a crate joins this set in the PR that
# sweeps it, and the unswept remainder is a shrinking crate list, not a rotting file allowlist.
#
# To sweep the next crate: add it here, run this linter, and work the list it prints — each entry is
# one comment to point, cut, or delete. That list is the worklist; nothing about the sweep needs to
# be discovered by reading. see rules: code-as-grounding
SWEPT_CRATES = {"crates/reuben-mcp"}

# The unpointed `//!` budget. Ten lines is room for what a module doc legitimately owes a reader —
# what this file is, and the local facts the code cannot state — before the length itself says
# rationale accumulated. Tuned on the reuben-mcp pilot: every swept module lands well under it,
# and the pre-sweep docs (25 lines in engine.rs, 42 in lib.rs) were all over.
MAX_UNPOINTED_MODULE_DOC = 10

# An issue citation in a comment: `#123`, or the cross-repo `reuben#123`. Two digits minimum, so
# `#3` in prose and a commented-out `#[derive(…)]` cannot trip it.
ISSUE_RE = re.compile(r"(?<![\w#])(?:[a-z][\w.-]*)?#\d{2,4}\b")
# A rule-level pointer: `agent-mcp.md#expect-guard-is-a-door-concern`. Code points at TOPICS only
# (docs/rules/README.md, Conventions) — a rule slug is the deepest rung and reworded freely, so a
# comment naming one is a link that breaks silently.
RULE_ANCHOR_RE = re.compile(r"\b[a-z0-9-]+\.md#[a-z0-9-]+")
# `//` and `//!` comment bodies. The lookbehind matters: without it this slides one character and
# matches the trailing `//` of a `///` doc comment, which check 4 deliberately does not reach.
NON_DOC_COMMENT_RE = re.compile(r"(?<!/)//(?!/)")


def module_doc_problems(rel: str, text: str) -> list[str]:
    """Check 3 for one Rust file: the leading `//!` block is short, or it names a topic.

    The block is the first run of `//!` lines, reached past blank lines and inner attributes
    (`#![…]`) — which is where rustfmt leaves a module doc — and ends at the first line that is
    neither.
    """
    doc: list[str] = []
    for line in text.splitlines():
        s = line.strip()
        if s.startswith("//!"):
            doc.append(s)
        elif doc:
            break                      # the block ended: first line of real code
        elif s and not s.startswith("#!"):
            break                      # code before any module doc: there is none
    if len(doc) <= MAX_UNPOINTED_MODULE_DOC or SEE_RE.search("\n".join(doc)):
        return []
    return [f"{rel}:1: {len(doc)}-line `//!` module doc names no topic — point the rationale at "
            f"one (`//! see rules: <topic>`) or cut it to mechanics "
            f"(<={MAX_UNPOINTED_MODULE_DOC} lines)"]


def comment_ref_problems(rel: str, text: str) -> list[str]:
    """Check 4 for one Rust file: a `//` or `//!` comment cites no issue and no rule anchor.

    Matches on the comment body only, so an issue number inside a string literal (a test fixture,
    a URL) is not a violation — the target is prose that retells history, not data.
    """
    problems = []
    for i, line in enumerate(text.splitlines(), 1):
        m = NON_DOC_COMMENT_RE.search(line)
        if not m:
            continue
        body = line[m.end():]
        for ref in ISSUE_RE.findall(body):
            problems.append(f"{rel}:{i}: issue citation `{ref}` in a comment — provenance belongs "
                            f"in a rationale file, not in code; point at a topic instead")
        for ref in RULE_ANCHOR_RE.findall(body):
            problems.append(f"{rel}:{i}: rule-level pointer `{ref}` in a comment — point at the "
                            f"topic instead (`see rules: <topic>`)")
    return problems


def main(root_arg: str = ".") -> int:
    root = Path(root_arg).resolve()
    errors: list[str] = []
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
        if path.suffix == ".rs" and any(rel.startswith(f"{c}/") for c in SWEPT_CRATES):
            errors.extend(module_doc_problems(rel, text))
            errors.extend(comment_ref_problems(rel, text))
        for i, line in enumerate(text.splitlines(), 1):
            if ADR_RE.search(line):
                errors.append(f"{rel}:{i}: ADR reference in code — point at a topic: `see rules: <topic>`")
            m = SEE_RE.search(line)
            if m:
                cross, slug = m.group(1), m.group(2)
                if not SLUG_RE.match(slug):
                    errors.append(f"{rel}:{i}: malformed rules slug '{slug}' (kebab-case expected)")
                elif not cross and not (root / "docs" / "rules" / f"{slug}.md").exists():
                    errors.append(f"{rel}:{i}: `see rules: {slug}` has no docs/rules/{slug}.md")
                elif cross and not (root / "engine" / "docs" / "rules" / f"{slug}.md").exists():
                    errors.append(f"{rel}:{i}: `see engine rules: {slug}` has no engine/docs/rules/{slug}.md")
    for e in errors:
        print(e, file=sys.stderr)
    print(f"check_rules_refs: {len(errors)} problem(s)", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:2]))

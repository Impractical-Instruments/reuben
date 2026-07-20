#!/usr/bin/env python3
"""Reference-linter for the rules-doc system. Runs in both engine and web repos.

Two checks:
  1. No `ADR-<n>` references survive in CODE. The only legitimate ADR mentions are
     `Distilled from:` lines in docs/rules/rationale/** and the live ADRs in docs/adr/**
     (both are Markdown, which this linter does not scan as code), plus the `absorb-adrs`
     skill itself — the tool that distils ADRs into rules and stamps each rationale's
     provenance line, so its scaffolder + tests name ADRs by design (see SKILL_ALLOWLIST).
  2. Every `see rules: <topic>` / `see engine rules: <topic>` code comment names a kebab-case
     slug; for the same-repo form, docs/rules/<topic>.md must exist. For the cross-repo form,
     the topic is resolved against the pinned engine submodule's engine/docs/rules/<topic>.md
     (the SHA web is built against) — a no-op in the engine repo, active once web bumps the pin.

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

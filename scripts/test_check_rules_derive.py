#!/usr/bin/env python3
"""Unit tests for check_rules_derive — the README derive guard + collator.

Covers the invariant every downstream authoring stage relies on: an empty/consistent index is
green under --check, a drifted one is red, --write collates it back to green, and structural
problems (missing summary, duplicate term) are reported rather than papered over.
"""
from __future__ import annotations
import tempfile
import unittest
from pathlib import Path

import check_rules_derive

README_SKELETON = """# reuben rules index

## Topics

<!-- derived — collated from each topic's `> summary`; do not hand-edit out of sync. -->
- **[<Topic title>](<topic>.md)** — <one-line summary>

## Glossary

<!-- derived — collated from each topic's `## Terms`, linking the defining topic. -->
- **<Term>** — <one-line definition> · [<topic>](<topic>.md)

## Conventions

Prose that must survive collation untouched.
"""

CLOCK = """# Clock

> How musical time works.

## Now

Time is a thing.

## Terms

- **Block** — a unit of time.
"""


def build(root: Path, topics: dict[str, str], readme: str = README_SKELETON):
    rules = root / "docs" / "rules"
    rules.mkdir(parents=True, exist_ok=True)
    (rules / "README.md").write_text(readme, encoding="utf-8")
    for name, body in topics.items():
        (rules / name).write_text(body, encoding="utf-8")
    return rules


class DeriveGuardTest(unittest.TestCase):
    def test_empty_tree_is_green_under_check(self):
        # README with empty derived sections, no topic docs.
        empty = README_SKELETON.replace(
            "- **[<Topic title>](<topic>.md)** — <one-line summary>\n", ""
        ).replace(
            "- **<Term>** — <one-line definition> · [<topic>](<topic>.md)\n", ""
        )
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {}, readme=empty)
            self.assertEqual(check_rules_derive.main(["--check", str(root)]), 0)

    def test_skeleton_with_topic_is_drifted_then_write_fixes_it(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {"clock.md": CLOCK})
            # Placeholder list items don't match the real topic -> drift, red.
            self.assertEqual(check_rules_derive.main(["--check", str(root)]), 1)
            # --write collates the real topic in...
            self.assertEqual(check_rules_derive.main(["--write", str(root)]), 0)
            # ...and now --check is green.
            self.assertEqual(check_rules_derive.main(["--check", str(root)]), 0)
            text = (root / "docs" / "rules" / "README.md").read_text(encoding="utf-8")
            self.assertIn("- **[Clock](clock.md)** — How musical time works.", text)
            self.assertIn("- **Block** — a unit of time. · [clock](clock.md)", text)
            self.assertIn("Prose that must survive collation untouched.", text)

    def test_write_is_idempotent(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {"clock.md": CLOCK})
            self.assertEqual(check_rules_derive.main(["--write", str(root)]), 0)
            first = (root / "docs" / "rules" / "README.md").read_text(encoding="utf-8")
            self.assertEqual(check_rules_derive.main(["--write", str(root)]), 0)
            second = (root / "docs" / "rules" / "README.md").read_text(encoding="utf-8")
            self.assertEqual(first, second)

    def test_missing_summary_is_structural_error(self):
        no_summary = "# Clock\n\n## Now\n\nNo summary line here.\n"
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {"clock.md": no_summary})
            # Structural problem: --check reports it, and --write refuses (returns 1).
            self.assertEqual(check_rules_derive.main(["--check", str(root)]), 1)
            self.assertEqual(check_rules_derive.main(["--write", str(root)]), 1)

    def test_duplicate_term_is_structural_error(self):
        dup_a = "# A\n\n> Topic A.\n\n## Terms\n\n- **Block** — from A.\n"
        dup_b = "# B\n\n> Topic B.\n\n## Terms\n\n- **Block** — from B.\n"
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {"a.md": dup_a, "b.md": dup_b})
            self.assertEqual(check_rules_derive.main(["--write", str(root)]), 1)
            self.assertEqual(check_rules_derive.main(["--check", str(root)]), 1)

    def test_readme_missing_returns_zero(self):
        # No docs/rules/README.md at all -> nothing to do, green.
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(check_rules_derive.main(["--check", d]), 0)


if __name__ == "__main__":
    unittest.main()

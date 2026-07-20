#!/usr/bin/env python3
"""Unit tests for check_rules_links — the rule<->rationale link guard.

Fixture trees are built with tempfile; the guard is imported as a bare module (tests run from
`scripts/`, mirroring the engine's skill-test idiom). Each test asserts the exact problem count
so a regression that over- or under-reports is caught, not just pass/fail.
"""
from __future__ import annotations
import tempfile
import unittest
from pathlib import Path

import check_rules_links


def build(root: Path, topics: dict[str, str], rationales=()):
    """Create docs/rules/ with a README and the given {name.md: body} topic docs, plus any
    rationale files (paths relative to docs/rules/)."""
    rules = root / "docs" / "rules"
    rules.mkdir(parents=True, exist_ok=True)
    (rules / "README.md").write_text("# reuben rules index\n", encoding="utf-8")
    for name, body in topics.items():
        (rules / name).write_text(body, encoding="utf-8")
    for rel in rationales:
        p = rules / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text("# Why\n", encoding="utf-8")


WELL_FORMED = """# Clock

> How musical time works.

## Now

Time is a thing.

## Rules

<a id="tempo-is-immutable"></a>
### Tempo is immutable within a block.

[why](rationale/clock/tempo-is-immutable.md)

## Terms

- **Block** — a unit of time.
"""


class LinksGuardTest(unittest.TestCase):
    def _problems(self, topics, rationales=()):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, topics, rationales)
            return check_rules_links.collect_problems(str(root))

    def test_empty_tree_is_green(self):
        # README only, no topic docs — the day-one invariant.
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {})
            self.assertEqual(check_rules_links.collect_problems(str(root)), [])

    def test_no_docs_rules_dir_is_green(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(check_rules_links.collect_problems(d), [])

    def test_well_formed_topic_is_green(self):
        problems = self._problems(
            {"clock.md": WELL_FORMED},
            rationales=["rationale/clock/tempo-is-immutable.md"],
        )
        self.assertEqual(problems, [])

    def test_anchor_with_no_why_link(self):
        body = """# Clock

> How musical time works.

## Rules

<a id="tempo-is-immutable"></a>
### Tempo is immutable within a block.
"""
        self.assertEqual(len(self._problems({"clock.md": body})), 1)

    def test_anchor_with_two_why_links(self):
        body = """# Clock

> How musical time works.

## Rules

<a id="tempo-is-immutable"></a>
### Tempo is immutable within a block.

[why](rationale/clock/tempo-is-immutable.md)
[why](rationale/clock/tempo-is-immutable.md)
"""
        self.assertEqual(
            len(self._problems(
                {"clock.md": body},
                rationales=["rationale/clock/tempo-is-immutable.md"],
            )),
            1,
        )

    def test_why_link_target_missing(self):
        # Single, well-formed [why] whose target file does not exist -> 1 problem.
        self.assertEqual(len(self._problems({"clock.md": WELL_FORMED})), 1)

    def test_topic_with_zero_rules(self):
        body = """# Clock

> How musical time works.

## Now

No rules here yet.

## Terms

- **Block** — a unit of time.
"""
        self.assertEqual(len(self._problems({"clock.md": body})), 1)

    def test_readme_and_subdirs_not_scanned(self):
        # A malformed template under _templates/ must not be scanned, and README is skipped.
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {"clock.md": WELL_FORMED},
                  rationales=["rationale/clock/tempo-is-immutable.md"])
            tmpl = root / "docs" / "rules" / "_templates"
            tmpl.mkdir(parents=True, exist_ok=True)
            (tmpl / "topic.md").write_text(
                '<a id="<rule-slug>"></a>\n### x\n', encoding="utf-8")
            self.assertEqual(check_rules_links.collect_problems(str(root)), [])

    # --- multi-rule-per-topic: exercise the anchor -> next-anchor span boundary ---
    # Every fixture above has a single anchor, so the boundary branch
    # (`next_anchor = anchors[idx+1][0] ...`) is only hit once real topics carry 2+ rules.
    # These fixtures put 2+ anchors under one `## Rules` and assert no span bleeds into its
    # neighbor.

    def test_multi_rule_topic_all_valid_is_green(self):
        body = """# Clock

> How musical time works.

## Rules

<a id="tempo-is-immutable"></a>
### Tempo is immutable within a block.

[why](rationale/clock/tempo-is-immutable.md)

<a id="block-is-atomic"></a>
### A block renders atomically.

[why](rationale/clock/block-is-atomic.md)

## Terms

- **Block** — a unit of time.
"""
        problems = self._problems(
            {"clock.md": body},
            rationales=[
                "rationale/clock/tempo-is-immutable.md",
                "rationale/clock/block-is-atomic.md",
            ],
        )
        self.assertEqual(problems, [])

    def test_multi_rule_first_missing_why_no_leakage(self):
        # First rule has NO [why]; second is well-formed. Correct span logic stops the first
        # rule at the second anchor, so the first counts 0 links -> exactly 1 problem, on the
        # first rule only. A boundary bug (span bleeding into the next rule) would let the
        # second rule's [why] satisfy the first and report 0 — this test catches that.
        body = """# Clock

> How musical time works.

## Rules

<a id="tempo-is-immutable"></a>
### Tempo is immutable within a block.

<a id="block-is-atomic"></a>
### A block renders atomically.

[why](rationale/clock/block-is-atomic.md)

## Terms

- **Block** — a unit of time.
"""
        problems = self._problems(
            {"clock.md": body},
            rationales=["rationale/clock/block-is-atomic.md"],
        )
        self.assertEqual(len(problems), 1)
        self.assertIn("tempo-is-immutable", problems[0])
        self.assertNotIn("block-is-atomic", problems[0])

    def test_multi_rule_first_has_two_whys_no_leakage(self):
        # First rule has TWO [why]; second is well-formed. Exactly 1 problem, attributed to the
        # first rule — the second must not be dragged into the first's over-count, nor vice versa.
        body = """# Clock

> How musical time works.

## Rules

<a id="tempo-is-immutable"></a>
### Tempo is immutable within a block.

[why](rationale/clock/tempo-is-immutable.md)
[why](rationale/clock/tempo-is-immutable.md)

<a id="block-is-atomic"></a>
### A block renders atomically.

[why](rationale/clock/block-is-atomic.md)

## Terms

- **Block** — a unit of time.
"""
        problems = self._problems(
            {"clock.md": body},
            rationales=[
                "rationale/clock/tempo-is-immutable.md",
                "rationale/clock/block-is-atomic.md",
            ],
        )
        self.assertEqual(len(problems), 1)
        self.assertIn("tempo-is-immutable", problems[0])
        self.assertNotIn("block-is-atomic", problems[0])


if __name__ == "__main__":
    unittest.main()

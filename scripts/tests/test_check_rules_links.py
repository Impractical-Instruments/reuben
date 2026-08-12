#!/usr/bin/env python3
"""Unit tests for check_rules_links — the corpus structural-integrity guard.

Fixture trees are built with tempfile; the guard is imported as a bare module (tests run from
`scripts/`, mirroring the engine's skill-test idiom). Each test asserts the exact problem count
so a regression that over- or under-reports is caught, not just pass/fail.
"""
from __future__ import annotations
import tempfile
import unittest
from pathlib import Path

from support import check_rules_links

# `ADR-<n>` is banned in code by check_rules_refs — provenance lives in a rationale file, not in a
# source file — and these fixtures are code. Composing the token keeps the fixture the real shape
# without planting a live reference for that linter to find.
PROVENANCE = f"Distilled from: ADR-{1:04d}"
# The minimum a rationale needs to satisfy checks (e) and (f), so a fixture aimed at one check
# does not trip the others.
RATIONALE_BODY = f"# Why\n\nBecause.\n\n{PROVENANCE}\n"


def build(root: Path, topics: dict[str, str], rationales=()):
    """Create docs/rules/ with a README and the given {name.md: body} topic docs, plus any
    rationale files. A rationale is either a path (given the default body) or a
    (path, body) pair; paths are relative to docs/rules/."""
    rules = root / "docs" / "rules"
    rules.mkdir(parents=True, exist_ok=True)
    (rules / "README.md").write_text("# rules index\n", encoding="utf-8")
    for name, body in topics.items():
        (rules / name).write_text(body, encoding="utf-8")
    for entry in rationales:
        rel, body = entry if isinstance(entry, tuple) else (entry, RATIONALE_BODY)
        p = rules / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(body, encoding="utf-8")


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


class CorpusIntegrityTest(unittest.TestCase):
    """Checks (d)-(f): the rungs below the topic docs, previously unwalked."""

    def _problems(self, topics, rationales=()):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, topics, rationales)
            return check_rules_links.collect_problems(str(root))

    def _clock(self, rationale_body):
        return self._problems(
            {"clock.md": WELL_FORMED},
            rationales=[("rationale/clock/tempo-is-immutable.md", rationale_body)],
        )

    # --- (d) link targets ---

    def test_rationale_link_to_missing_file(self):
        # The defect this check exists for: an absorption pass deletes a rule and its rationale,
        # and a sibling rationale still links the file. Previously green.
        problems = self._clock(
            f"# Why\n\nSee [gone](gone.md).\n\n{PROVENANCE}\n")
        self.assertEqual(len(problems), 1)
        self.assertIn("gone.md", problems[0])

    def test_rationale_link_to_missing_anchor(self):
        # The file resolves but the rule slug inside it does not — the half of the same defect
        # that survives when only the anchor is renamed.
        problems = self._clock(
            f"# Why\n\n[Rule](../../clock.md#renamed-away)\n\n{PROVENANCE}\n")
        self.assertEqual(len(problems), 1)
        self.assertIn("renamed-away", problems[0])

    def test_rationale_backlink_to_real_anchor_is_green(self):
        self.assertEqual(
            self._clock(
                f"# Why\n\n[Rule](../../clock.md#tempo-is-immutable)\n\n{PROVENANCE}\n"),
            [],
        )

    def test_link_inside_inline_code_is_not_a_link(self):
        # A rationale quoting the Markdown a doc comment shipped is prose about markup, not a
        # link. Reading it as one is a false positive that would force a bogus doc edit.
        self.assertEqual(
            self._clock(
                "# Why\n\nIt shipped `[`projection`](crate::projection)` to a model.\n\n"
                f"{PROVENANCE}\n"),
            [],
        )

    def test_link_inside_fenced_block_is_not_a_link(self):
        self.assertEqual(
            self._clock(
                f"# Why\n\n```md\n[example](nowhere.md)\n```\n\n{PROVENANCE}\n"),
            [],
        )

    def test_external_link_is_not_resolved(self):
        self.assertEqual(
            self._clock(
                f"# Why\n\n[issue](https://example.test/1)\n\n{PROVENANCE}\n"),
            [],
        )

    def test_missing_why_target_reports_once_not_twice(self):
        # Check (c) owns a topic's [why] links; check (d) must not report the same break again.
        problems = self._problems({"clock.md": WELL_FORMED})
        self.assertEqual(len(problems), 1)
        self.assertIn("[why]", problems[0])

    # --- (e) bijection ---

    def test_orphan_rationale(self):
        problems = self._problems(
            {"clock.md": WELL_FORMED},
            rationales=["rationale/clock/tempo-is-immutable.md",
                        "rationale/clock/left-behind.md"],
        )
        self.assertEqual(len(problems), 1)
        self.assertIn("left-behind.md", problems[0])
        self.assertIn("orphan", problems[0])

    def test_rationale_shared_by_two_rules(self):
        body = """# Clock

> How musical time works.

## Rules

<a id="tempo-is-immutable"></a>
### Tempo is immutable within a block.

[why](rationale/clock/tempo-is-immutable.md)

<a id="block-is-atomic"></a>
### A block renders atomically.

[why](rationale/clock/tempo-is-immutable.md)
"""
        problems = self._problems(
            {"clock.md": body}, rationales=["rationale/clock/tempo-is-immutable.md"])
        self.assertEqual(len(problems), 1)
        self.assertIn("linked by 2 rules", problems[0])

    def test_rationale_under_a_directory_that_is_not_a_topic(self):
        # A topic doc renamed without moving its rationale directory.
        body = WELL_FORMED.replace(
            "rationale/clock/tempo-is-immutable.md", "rationale/timing/tempo-is-immutable.md")
        problems = self._problems(
            {"clock.md": body}, rationales=["rationale/timing/tempo-is-immutable.md"])
        self.assertEqual(len(problems), 1)
        self.assertIn("not a topic doc", problems[0])

    # --- (f) provenance ---

    def test_rationale_without_provenance(self):
        problems = self._clock("# Why\n\nBecause.\n")
        self.assertEqual(len(problems), 1)
        self.assertIn("provenance", problems[0])

    def test_decided_in_satisfies_provenance(self):
        self.assertEqual(
            self._clock("# Why\n\nBecause.\n\nDecided in: issue #12 — settled directly, no ADR.\n"),
            [],
        )


class SupersessionTest(unittest.TestCase):
    """Checks (g) + (h): a rule an ADR has overturned says so, and the marker dies with the ADR.

    The live corpus has no ADRs, so this class is the only place these two run against real
    input. Every `ADR-<n>` token is composed rather than written, for the same reason the module
    header explains — these fixtures are code, and code does not name ADRs.
    """

    ADR = f"ADR-{1:04d}"
    RATIONALE = ["rationale/clock/tempo-is-immutable.md"]

    def _problems(self, adrs: dict[str, str], topic: str = WELL_FORMED):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {"clock.md": topic}, self.RATIONALE)
            adr_dir = root / "docs" / "adr"
            adr_dir.mkdir(parents=True, exist_ok=True)
            for name, body in adrs.items():
                (adr_dir / name).write_text(body, encoding="utf-8")
            return check_rules_links.collect_problems(str(root))

    def marked(self, adr: str | None = None) -> str:
        """WELL_FORMED with the supersession marker on its one rule."""
        marker = f"Superseded by: {adr or self.ADR} (pending absorption)"
        return WELL_FORMED.replace("### Tempo is immutable within a block.\n",
                                   f"### Tempo is immutable within a block.\n\n{marker}\n")

    # --- (g) an ADR that overturns a rule marks it ---

    def test_an_adr_naming_an_unmarked_rule_fails(self):
        problems = self._problems({"0001-tempo.md": "# Tempo\n\nOverturns `clock.md#tempo-is-immutable`.\n"})
        self.assertEqual(len(problems), 1)
        self.assertIn("no marker", problems[0])
        self.assertIn(self.ADR, problems[0])

    def test_a_marked_rule_satisfies_the_adr(self):
        self.assertEqual(
            self._problems({"0001-tempo.md": "# Tempo\n\nOverturns [it](../rules/clock.md#tempo-is-immutable).\n"},
                           topic=self.marked()),
            [])

    def test_an_adr_naming_only_a_topic_trips_nothing(self):
        # The escape hatch, and the reason naming an anchor can be read as overturning it: an ADR
        # that wants context points at the topic, the way every other pointer in the corpus does.
        self.assertEqual(
            self._problems({"0001-tempo.md": "# Tempo\n\nSee [clock](../rules/clock.md).\n"}), [])

    def test_the_same_anchor_named_twice_is_one_finding(self):
        body = ("# Tempo\n\nOverturns `clock.md#tempo-is-immutable`.\n\n"
                "As established, `clock.md#tempo-is-immutable` no longer holds.\n")
        self.assertEqual(len(self._problems({"0001-tempo.md": body})), 1)

    def test_an_anchor_inside_a_fenced_block_is_not_a_claim(self):
        body = "# Tempo\n\n```md\n[why](clock.md#tempo-is-immutable)\n```\n"
        self.assertEqual(self._problems({"0001-tempo.md": body}), [])

    def test_the_adr_readme_is_not_an_adr(self):
        # It is the surface's own prose; it explains the lifecycle and may name anything.
        self.assertEqual(
            self._problems({"README.md": "# ADRs\n\ne.g. `clock.md#tempo-is-immutable`.\n"}), [])

    def test_an_adr_naming_an_unknown_topic_fails(self):
        problems = self._problems({"0001-tempo.md": "# T\n\nOverturns `nope.md#tempo-is-immutable`.\n"})
        self.assertEqual(len(problems), 1)
        self.assertIn("not a topic doc", problems[0])

    def test_an_adr_naming_an_unknown_anchor_fails(self):
        problems = self._problems({"0001-tempo.md": "# T\n\nOverturns `clock.md#no-such-rule`.\n"})
        self.assertEqual(len(problems), 1)
        self.assertIn("not a rule anchor", problems[0])

    # --- (h) the marker dies with the ADR ---

    def test_a_marker_naming_a_dead_adr_fails(self):
        # What absorption leaves behind: the ADR is deleted, the rule still claims to be pending.
        problems = self._problems({}, topic=self.marked())
        self.assertEqual(len(problems), 1)
        self.assertIn("not a live ADR", problems[0])

    def test_a_marker_naming_a_different_live_adr_still_fails(self):
        problems = self._problems({"0001-tempo.md": "# T\n\nSee [clock](../rules/clock.md).\n"},
                                  topic=self.marked(adr=f"ADR-{9:04d}"))
        self.assertEqual(len(problems), 1)
        self.assertIn("not a live ADR", problems[0])


if __name__ == "__main__":
    unittest.main()

# ii:begin provenance — derived from .ii/repo.toml; do not hand-edit out of sync. Regenerate with `python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .`. sha256=78bcd1ad231bbf04b478adf8f4745a330844b403cd85511924d62e3fa2aa307f
# Source:   Impractical-Instruments/agent-tools@057c3f7a9391816263b1a4fcb46af5f4a5dc705f:plugins/impractical-doctrine/rules/tests/test_check_rules_links.py
# Fetched:  2026-08-12
# Refresh:  gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/rules/tests/test_check_rules_links.py?ref=main' --jq '.content' | base64 -d > scripts/tests/test_check_rules_links.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .
# Do not edit locally. Changes go upstream via PR against the source repo.
# ii:end provenance

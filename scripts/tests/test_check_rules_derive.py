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

from support import check_rules_derive

README_SKELETON = """# rules index

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
        # Populated README (>=1 topic): the case that already worked — guard against regression.
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {"clock.md": CLOCK})
            self.assertEqual(check_rules_derive.main(["--write", str(root)]), 0)
            first = (root / "docs" / "rules" / "README.md").read_text(encoding="utf-8")
            self.assertEqual(check_rules_derive.main(["--write", str(root)]), 0)
            second = (root / "docs" / "rules" / "README.md").read_text(encoding="utf-8")
            self.assertEqual(first, second)

    def test_write_empty_section_is_idempotent(self):
        # Day-one README: no topic docs, so both derived sections collate to empty. This is the
        # case the old splice() grew by a blank line on every run.
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {})  # skeleton with placeholder items, zero topics
            self.assertEqual(check_rules_derive.main(["--write", str(root)]), 0)
            first = (root / "docs" / "rules" / "README.md").read_text(encoding="utf-8")
            self.assertEqual(check_rules_derive.main(["--write", str(root)]), 0)
            second = (root / "docs" / "rules" / "README.md").read_text(encoding="utf-8")
            self.assertEqual(first, second, "empty-section --write must be byte-idempotent")
            # And the empty index is still green under --check.
            self.assertEqual(check_rules_derive.main(["--check", str(root)]), 0)

    def test_write_eof_section_is_idempotent(self):
        # A derived section that is the LAST thing in the file (no following `## ` heading) must
        # round-trip identically — exercises the EOF branch of splice().
        readme_eof = """# rules index

## Topics

<!-- derived topics -->
- **[<Topic title>](<topic>.md)** — <one-line summary>

## Glossary

<!-- derived glossary -->
- **<Term>** — <one-line definition> · [<topic>](<topic>.md)
"""
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {"clock.md": CLOCK}, readme=readme_eof)
            self.assertEqual(check_rules_derive.main(["--write", str(root)]), 0)
            first = (root / "docs" / "rules" / "README.md").read_text(encoding="utf-8")
            self.assertEqual(check_rules_derive.main(["--write", str(root)]), 0)
            second = (root / "docs" / "rules" / "README.md").read_text(encoding="utf-8")
            self.assertEqual(first, second, "at-EOF --write must be byte-idempotent")
            self.assertEqual(check_rules_derive.main(["--check", str(root)]), 0)
            # The Glossary (last section) still collated correctly.
            self.assertIn("- **Block** — a unit of time. · [clock](clock.md)", second)

    def test_multiline_leading_comment_preserved_and_idempotent(self):
        # A leading HTML comment wrapped across lines must survive `--write` intact — opener, body,
        # and closing `-->` all retained, no dangling/unterminated `<!--` eating the entries. (Fails
        # against the single-line-only comment loop.)
        readme = """# rules index

## Topics

<!--
  derived — collated from each topic's `> summary`; do not hand-edit out of sync.
-->
- **[<Topic title>](<topic>.md)** — <one-line summary>

## Glossary

<!-- derived glossary -->
- **<Term>** — <one-line definition> · [<topic>](<topic>.md)

## Conventions

Prose that must survive collation untouched.
"""
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {"clock.md": CLOCK}, readme=readme)
            self.assertEqual(check_rules_derive.main(["--write", str(root)]), 0)
            first = (root / "docs" / "rules" / "README.md").read_text(encoding="utf-8")
            # Multi-line comment preserved verbatim, and every `<!--` has a matching `-->`.
            self.assertIn(
                "<!--\n  derived — collated from each topic's `> summary`; "
                "do not hand-edit out of sync.\n-->",
                first,
            )
            self.assertEqual(first.count("<!--"), first.count("-->"),
                             "unbalanced HTML comment markers -> corruption")
            # Entries collated after the (intact) comment.
            self.assertIn("- **[Clock](clock.md)** — How musical time works.", first)
            self.assertIn("Prose that must survive collation untouched.", first)
            # Idempotent on a second write; still green under --check.
            self.assertEqual(check_rules_derive.main(["--write", str(root)]), 0)
            second = (root / "docs" / "rules" / "README.md").read_text(encoding="utf-8")
            self.assertEqual(first, second, "multi-line-comment --write must be byte-idempotent")
            self.assertEqual(check_rules_derive.main(["--check", str(root)]), 0)

    def test_summary_with_leading_gt_preserves_inner_marker(self):
        # `str.lstrip("> ")` would strip the run `> >` and drop the inner `>`; only the one
        # blockquote marker should be removed.
        topic = "# Clock\n\n> >50% of blocks share a tempo.\n\n## Terms\n\n- **Block** — a unit.\n"
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {"clock.md": topic})
            self.assertEqual(check_rules_derive.main(["--write", str(root)]), 0)
            text = (root / "docs" / "rules" / "README.md").read_text(encoding="utf-8")
            self.assertIn("- **[Clock](clock.md)** — >50% of blocks share a tempo.", text)
            self.assertEqual(check_rules_derive.main(["--check", str(root)]), 0)

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

    def test_write_refuses_when_a_collated_section_has_no_heading(self):
        # The trap this closes: `splice` can only rewrite a section that is there, so an index
        # with no `## Glossary` and a topic that defines terms used to leave `--write` exiting 0
        # having written nothing, while `--check` stayed red forever. The pre-commit hook passes,
        # CI reds, and the per-term message names no remedy. `--write` now refuses and says where.
        no_glossary = "# rules index\n\n## Topics\n\n"
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {"clock.md": CLOCK}, readme=no_glossary)
            self.assertEqual(check_rules_derive.main(["--write", str(root)]), 1)
            self.assertEqual(check_rules_derive.main(["--check", str(root)]), 1)
            # And it did not paper over the absence by inventing a heading in a hand-written index.
            self.assertNotIn("## Glossary", (root / "docs" / "rules" / "README.md").read_text())

    def test_an_index_that_collates_nothing_may_omit_the_heading(self):
        # The other side of the same rule, and a live case: an index whose topics define no terms
        # carries no `## Glossary` on purpose. Refusing here would red a repo for a section it was
        # right not to have.
        no_terms = "# Clock\n\n> How musical time works.\n\n## Now\n\nTime is a thing.\n"
        no_glossary = "# rules index\n\n## Topics\n\n"
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {"clock.md": no_terms}, readme=no_glossary)
            self.assertEqual(check_rules_derive.main(["--write", str(root)]), 0)
            self.assertEqual(check_rules_derive.main(["--check", str(root)]), 0)
            self.assertIn("- **[Clock](clock.md)** — How musical time works.",
                          (root / "docs" / "rules" / "README.md").read_text())

    def test_a_fenced_heading_is_an_example_not_a_section(self):
        # An index that documents its own format carries `## Glossary` inside a fence. `splice` used
        # to take that for the real section, rewrite from there to the next real heading, and eat
        # the closing fence and everything after it — and both modes then called the wreckage
        # clean. The blast radius was every line below the example, not the example.
        documented = """# rules index

## Topics

Example of what an index looks like:

```
## Glossary

<!-- derived -->
- **<Term>** — <one-line definition> · [<topic>](<topic>.md)
```

Prose below the fence, which must survive.
"""
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {"clock.md": CLOCK}, readme=documented)
            readme = root / "docs" / "rules" / "README.md"
            # There is no real `## Glossary`, so this is the refusal case — not a splice into the
            # example.
            self.assertEqual(check_rules_derive.main(["--write", str(root)]), 1)
            self.assertEqual(readme.read_text(encoding="utf-8"), documented,
                             "a refused --write must leave the file byte-identical")

    def test_a_fenced_heading_does_not_swallow_the_section_it_sits_in(self):
        # The same mask on the path that does write, in the shape a real index has: the derived
        # sections first, then a hand-written `## Conventions` quoting the format. The fenced
        # `## Topics` inside it must not read as a section boundary — if it did, the collator would
        # rewrite from there and take the closing fence and the rest of the file with it.
        #
        # Content directly under a derived heading with no following `## ` IS that section's body
        # and is replaced; that is the design, not this bug. Which is why the example lives under a
        # heading of its own here.
        documented = """# rules index

## Topics

## Glossary

## Conventions

Example of what the derived sections look like:

```
## Topics
- **<Term>** — <one-line definition>
```

Prose below the fence, which must survive.
"""
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {"clock.md": CLOCK}, readme=documented)
            readme = root / "docs" / "rules" / "README.md"
            self.assertEqual(check_rules_derive.main(["--write", str(root)]), 0)
            out = readme.read_text(encoding="utf-8")
            self.assertIn("Prose below the fence, which must survive.", out)
            self.assertEqual(out.count("```"), 2, "the fence must still be closed")
            self.assertIn("- **[Clock](clock.md)** — How musical time works.", out)
            self.assertIn("- **Block** — a unit of time. · [clock](clock.md)", out)
            self.assertEqual(check_rules_derive.main(["--check", str(root)]), 0)

    def test_check_does_not_read_a_fenced_example_as_a_real_entry(self):
        # The `--check` side of the same rule. A placeholder inside a fence is not a term the index
        # claims to define, so it must not be reported as one defined in no topic.
        documented = """# rules index

## Topics

- **[Clock](clock.md)** — How musical time works.

## Glossary

- **Block** — a unit of time. · [clock](clock.md)

```
- **Phantom** — a term that exists only in an example. · [nowhere](nowhere.md)
```
"""
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {"clock.md": CLOCK}, readme=documented)
            self.assertEqual(check_rules_derive.main(["--check", str(root)]), 0)

    def test_a_fenced_terms_section_in_a_topic_is_not_a_terms_section(self):
        # Topic docs are scanned by the same rule: the templates a topic quotes are examples.
        quoting = """# Clock

> How musical time works.

## Now

A topic doc may quote the shape it follows:

```
## Terms

- **<Term>** — <one-line definition.>
```

## Terms

- **Block** — a unit of time.
"""
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {"clock.md": quoting})
            self.assertEqual(check_rules_derive.main(["--write", str(root)]), 0)
            out = (root / "docs" / "rules" / "README.md").read_text(encoding="utf-8")
            self.assertIn("- **Block** — a unit of time. · [clock](clock.md)", out)
            self.assertNotIn("<Term>", out)

    def test_readme_missing_returns_zero(self):
        # No docs/rules/README.md at all -> nothing to do, green.
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(check_rules_derive.main(["--check", d]), 0)


if __name__ == "__main__":
    unittest.main()

# ii:begin provenance — derived from .ii/repo.toml; do not hand-edit out of sync. Regenerate with `python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .`. sha256=2cdaba37e8301f1d0336cb9508ca921235774ea7e03f7e33f429c03aa83b60f6
# Source:   Impractical-Instruments/agent-tools@057c3f7a9391816263b1a4fcb46af5f4a5dc705f:plugins/impractical-doctrine/rules/tests/test_check_rules_derive.py
# Fetched:  2026-08-12
# Refresh:  gh api 'repos/Impractical-Instruments/agent-tools/contents/plugins/impractical-doctrine/rules/tests/test_check_rules_derive.py?ref=main' --jq '.content' | base64 -d > scripts/tests/test_check_rules_derive.py && python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .
# Do not edit locally. Changes go upstream via PR against the source repo.
# ii:end provenance

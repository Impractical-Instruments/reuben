#!/usr/bin/env python3
"""Unit tests for check_rules_refs — the ADR/pointer reference-linter and the module-doc guard.

Fixture trees are built with tempfile; the guard is imported as a bare module (tests run from
`scripts/`, mirroring the sibling link-guard tests). Each test asserts the exact problem count so a
regression that over- or under-reports is caught, not just pass/fail.

`module_doc_problems` is exercised directly on source text — no tree needed — and the swept-crate
gating is exercised through `main`, since which paths check 3 reaches is the part that can regress.
"""
from __future__ import annotations
import tempfile
import unittest
from pathlib import Path

import check_rules_refs
from check_rules_refs import MAX_UNPOINTED_MODULE_DOC as BUDGET


def doc(lines: int, pointer: bool = False) -> str:
    """A Rust file whose module doc is `lines` long, optionally naming a topic on its last line."""
    body = [f"//! line {i}" for i in range(lines - (1 if pointer else 0))]
    if pointer:
        body.append("//! see rules: code-as-grounding")
    return "\n".join(body) + "\n\npub fn f() {}\n"


def build(root: Path, files: dict[str, str]) -> None:
    """Write {repo-relative path: contents}, plus the docs/rules/ topic the pointers resolve to."""
    topic = root / "docs" / "rules" / "code-as-grounding.md"
    topic.parent.mkdir(parents=True, exist_ok=True)
    topic.write_text("# Code as a grounding surface\n", encoding="utf-8")
    for rel, text in files.items():
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text, encoding="utf-8")


class ModuleDocGuard(unittest.TestCase):
    def test_a_doc_within_budget_needs_no_pointer(self):
        self.assertEqual(check_rules_refs.module_doc_problems("a.rs", doc(BUDGET)), [])

    def test_a_doc_over_budget_without_a_pointer_is_flagged(self):
        problems = check_rules_refs.module_doc_problems("a.rs", doc(BUDGET + 1))
        self.assertEqual(len(problems), 1)
        self.assertIn(f"{BUDGET + 1}-line", problems[0])
        self.assertTrue(problems[0].startswith("a.rs:1:"))

    def test_a_pointer_licenses_any_length(self):
        self.assertEqual(
            check_rules_refs.module_doc_problems("a.rs", doc(BUDGET * 5, pointer=True)), []
        )

    def test_the_block_is_the_leading_run_only(self):
        # `//!` lines further down (a nested `mod` with its own doc) are a different block and do
        # not accumulate into this file's budget.
        text = doc(3) + "\nmod inner {\n" + "".join(f"    //! deep {i}\n" for i in range(20)) + "}\n"
        self.assertEqual(check_rules_refs.module_doc_problems("a.rs", text), [])

    def test_an_inner_attribute_does_not_end_the_search(self):
        # rustfmt leaves `#![…]` above or below the module doc; either way the doc is still found.
        text = "#![allow(clippy::too_many_lines)]\n\n" + doc(BUDGET + 1)
        self.assertEqual(len(check_rules_refs.module_doc_problems("a.rs", text)), 1)

    def test_a_file_with_no_module_doc_is_clean(self):
        self.assertEqual(check_rules_refs.module_doc_problems("a.rs", "pub fn f() {}\n"), [])

    def test_item_docs_are_not_module_docs(self):
        # A long `///` on the first item is signature-level mechanics, which the guard never counts.
        text = "".join(f"/// line {i}\n" for i in range(40)) + "pub fn f() {}\n"
        self.assertEqual(check_rules_refs.module_doc_problems("a.rs", text), [])


class CommentRefGuard(unittest.TestCase):
    def problems(self, line: str) -> list[str]:
        return check_rules_refs.comment_ref_problems("a.rs", line + "\n")

    def test_a_line_comment_citing_an_issue_is_flagged(self):
        self.assertEqual(len(self.problems("    // fixed in #604, see there")), 1)

    def test_a_module_doc_citing_an_issue_is_flagged(self):
        self.assertEqual(len(self.problems("//! the structure channel (#315)")), 1)

    def test_the_cross_repo_spelling_is_flagged(self):
        problems = self.problems("    // reuben#459 tightened this")
        self.assertEqual(len(problems), 1)
        self.assertIn("reuben#459", problems[0])

    def test_a_doc_comment_is_out_of_reach(self):
        # Regression: a naive `//(?!/)` slides one char and matches the trailing `//` of `///`.
        # `///` on a JsonSchema type is the advertised schema description, not comment prose.
        self.assertEqual(self.problems("/// retired with #604 — a model that held one"), [])

    def test_a_string_literal_is_not_a_comment(self):
        self.assertEqual(self.problems('    let s = "regression for #604";'), [])

    def test_an_attribute_is_not_an_issue_ref(self):
        self.assertEqual(self.problems("    // #[derive(Debug)] was removed here"), [])

    def test_a_single_digit_reference_is_not_an_issue_ref(self):
        # `# 3` / `#3` show up in prose (a numbered list, a heading); two digits minimum.
        self.assertEqual(self.problems("    // step #3 of the handshake"), [])

    def test_a_topic_pointer_is_clean(self):
        self.assertEqual(self.problems("//! see rules: agent-mcp"), [])

    def test_a_rule_anchor_is_flagged(self):
        # docs/rules/README.md Conventions: code points at topics, never at a rule slug.
        problems = self.problems("    // see agent-mcp.md#expect-guard-is-a-door-concern")
        self.assertEqual(len(problems), 1)
        self.assertIn("rule-level pointer", problems[0])


class SweptCrateGating(unittest.TestCase):
    def run_main(self, files: dict[str, str]) -> int:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            build(root, files)
            return check_rules_refs.main(str(root))

    def test_an_unswept_crate_is_not_checked(self):
        over = doc(BUDGET + 5)
        self.assertEqual(self.run_main({"crates/reuben-core/src/lib.rs": over}), 0)

    def test_a_swept_crate_is_checked(self):
        over = doc(BUDGET + 5)
        self.assertEqual(self.run_main({"crates/reuben-mcp/src/lib.rs": over}), 1)

    def test_the_swept_set_names_crates_that_exist(self):
        # A typo'd or renamed crate silently disables the gate for it, which is the one failure
        # this ratchet cannot survive — so pin the set against the real tree.
        repo = Path(__file__).resolve().parent.parent
        for crate in check_rules_refs.SWEPT_CRATES:
            self.assertTrue((repo / crate / "Cargo.toml").is_file(), f"no such crate: {crate}")


if __name__ == "__main__":
    unittest.main()

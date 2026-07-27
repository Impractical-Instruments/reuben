#!/usr/bin/env python3
"""Unit tests for check_rules_refs — the ADR/pointer reference-linter and the module-doc guard.

Fixture trees are built with tempfile; the guard is imported as a bare module (tests run from
`scripts/`, mirroring the sibling link-guard tests). Each test asserts the exact problem count so a
regression that over- or under-reports is caught, not just pass/fail.

`module_doc_problems` is exercised directly on source text — no tree needed — and `main` is
exercised through a tree, since which paths check 3 reaches is the part that can regress.
"""
from __future__ import annotations
import tempfile
import unittest
from pathlib import Path

import check_rules_refs

BUDGET = check_rules_refs.MAX_UNPOINTED_MODULE_DOC


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

    def test_a_line_comment_above_the_doc_does_not_hide_it(self):
        # A `//` header above the module doc must not put the doc out of the scan's reach —
        # otherwise one line anywhere on top silently exempts a file from the budget.
        text = "// generated; do not edit\n" + doc(BUDGET + 1)
        self.assertEqual(len(check_rules_refs.module_doc_problems("a.rs", text)), 1)

    def test_a_block_comment_above_the_doc_does_not_hide_it(self):
        text = "/* licence\n   header */\n\n" + doc(BUDGET + 1)
        self.assertEqual(len(check_rules_refs.module_doc_problems("a.rs", text)), 1)

    def test_a_wrapped_inner_attribute_does_not_hide_the_doc(self):
        # `#![…]` wraps across lines; only its first line starts with `#!`.
        text = "#![allow(\n    clippy::too_many_lines,\n)]\n\n" + doc(BUDGET + 1)
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

    def test_a_url_in_a_string_literal_is_not_a_comment(self):
        # The `//` of a scheme is the case a regex cannot get right: it opens no comment, so
        # neither the fragment nor the issue number after it is prose.
        self.assertEqual(self.problems('    const D: &str = "https://x/agent-mcp.md#1234";'), [])

    def test_a_comment_after_a_string_literal_is_still_reached(self):
        # The converse: skipping string literals must not skip the real comment behind one.
        self.assertEqual(len(self.problems('    let s = "https://x"; // fixed in #604')), 1)

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


class PointerGuard(unittest.TestCase):
    """Checks 2 + 5: a pointer resolves to a topic doc, and names nothing deeper than one."""

    def problems(self, text: str, topics=("code-as-grounding", "agent-mcp")) -> list[str]:
        return check_rules_refs.pointer_problems(
            "a.rs", text + "\n", lambda cross, slug: not cross and slug in topics
        )

    def test_a_topic_pointer_is_clean(self):
        self.assertEqual(self.problems("//! see rules: code-as-grounding"), [])

    def test_an_unknown_topic_is_flagged(self):
        problems = self.problems("//! see rules: no-such-topic")
        self.assertEqual(len(problems), 1)
        self.assertIn("docs/rules/no-such-topic.md", problems[0])

    def test_a_capitalised_pointer_is_flagged(self):
        # A capitalised pointer is invisible to SEE_RE, so its topic was never validated at all.
        problems = self.problems("//! See rules: code-as-grounding")
        self.assertEqual(len(problems), 1)
        self.assertIn("capitalised", problems[0])

    def test_a_parenthesised_rule_slug_is_flagged(self):
        problems = self.problems("/// held by a door — see rules: agent-mcp\n"
                                 "/// (expect-guard-is-a-door-concern).")
        self.assertEqual(len(problems), 1)
        self.assertIn("expect-guard-is-a-door-concern", problems[0])

    def test_a_comma_continued_rule_slug_is_flagged(self):
        problems = self.problems("//! the lane cut (see rules: agent-mcp,\n//! cross-lane-grounding).")
        self.assertEqual(len(problems), 1)
        self.assertIn("cross-lane-grounding", problems[0])

    def test_a_topic_anchor_is_flagged(self):
        # The `<topic>#<rule>` spelling, which check 4's `<file>.md#<rule>` regex does not see.
        problems = self.problems("    // see rules: agent-mcp#expect-guard-is-a-door-concern")
        self.assertEqual(len(problems), 1)
        self.assertIn("reaches past its topic", problems[0])

    def test_a_doc_comment_is_in_reach(self):
        # Unlike check 4, check 5 reads `///`: a rule slug is a broken link on any comment.
        self.assertEqual(len(self.problems("/// see rules: agent-mcp (portable-tool-contracts).")), 1)

    def test_a_pointer_wrapped_before_its_topic_still_resolves(self):
        # rustfmt breaks the line wherever the column runs out; the topic is still validated.
        self.assertEqual(self.problems("//! the seam. see rules:\n//! code-as-grounding"), [])

    def test_a_wrapped_pointer_to_an_unknown_topic_is_flagged(self):
        self.assertEqual(len(self.problems("//! the seam. see rules:\n//! no-such-topic")), 1)

    def test_prose_after_the_topic_is_not_a_rule_slug(self):
        # Only a paren group, comma list, or `#` anchor opening right after the topic is a
        # pointer; ordinary prose that happens to follow is not.
        self.assertEqual(self.problems("/// check — see rules: agent-mcp. One exception, local"), [])

    def test_a_second_topic_after_a_comma_is_clean(self):
        self.assertEqual(self.problems("//! see rules: agent-mcp, code-as-grounding"), [])

    def test_a_pointer_in_a_string_literal_is_not_prose(self):
        # This file's own fixtures are exactly that: a pointer held as data, not written as one.
        self.assertEqual(self.problems('    let s = "see rules: no-such-topic";'), [])

    def test_a_hash_language_reads_hash_comments_only(self):
        # In a `#` file the `//` of a fixture string opens nothing, and a single-quoted literal is
        # as opaque as a double-quoted one.
        text = "s = '// see rules: no-such-topic'\n# see rules: code-as-grounding"
        self.assertEqual(
            check_rules_refs.pointer_problems(
                "a.py", text + "\n", lambda cross, slug: slug == "code-as-grounding", "#"
            ),
            [],
        )


class ParityGuard(unittest.TestCase):
    """Check 6: a parity marker that exists records a substantive reason.

    The marker is assembled rather than written literally — this guard scans its own tree, and a
    fixture spelled out in a `#` comment here would be a real finding in `scripts/`.
    """

    MARK = "Parity" + ":"

    def problems(self, text: str) -> list[str]:
        return check_rules_refs.parity_problems("a.rs", text + "\n")

    def test_a_marker_with_a_real_reason_is_clean(self):
        self.assertEqual(self.problems(
            f"/// {self.MARK} the door builds this list at runtime, so the test is the only "
            f"place both exist."), [])

    def test_parity_marker_without_a_reason_fails(self):
        # The rubber stamp this check exists to reject: a marker, and nothing recorded.
        problems = self.problems(f"    // {self.MARK} n/a")
        self.assertEqual(len(problems), 1)
        self.assertIn("records no reason", problems[0])

    def test_a_bare_marker_fails(self):
        self.assertEqual(len(self.problems(f"    // {self.MARK}")), 1)

    def test_a_mis_cased_marker_is_flagged_not_skipped(self):
        # Same posture as check 5's capitalised pointer: the wrong spelling is unvalidated, which
        # reads as a clean file unless it is reported.
        problems = self.problems(f"    // {self.MARK.lower()} the wire response only exists here")
        self.assertEqual(len(problems), 1)
        self.assertIn("mis-cased", problems[0])

    def test_a_reason_wrapped_across_lines_is_counted_whole(self):
        # rustfmt breaks the line wherever the column runs out; the reason is still one sentence.
        self.assertEqual(self.problems(f"    // {self.MARK} the door builds\n"
                                       f"    // this list at runtime"), [])

    def test_a_marker_in_a_string_literal_is_not_prose(self):
        self.assertEqual(self.problems(f'    let s = "{self.MARK} n/a";'), [])

    def test_a_file_with_no_marker_is_clean(self):
        self.assertEqual(self.problems("pub fn f() {}"), [])

    def test_an_unmarked_parity_test_is_not_a_finding(self):
        # The undecidable half, asserted so the guard's reach stays honest: this shape is a
        # round-trip everywhere it appears in this workspace, and flagging it would be noise.
        self.assertEqual(self.problems("#[test]\nfn t() { assert_eq!(a.len(), b.len()); }"), [])


class WholeTree(unittest.TestCase):
    def run_main(self, files: dict[str, str]) -> int:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            build(root, files)
            return check_rules_refs.main(str(root))

    def test_a_rust_file_anywhere_is_checked(self):
        over = doc(BUDGET + 5)
        self.assertEqual(self.run_main({"crates/reuben-mcp/src/lib.rs": over}), 1)

    def test_a_non_rust_file_is_not_module_doc_checked(self):
        # Checks 3 and 4 read Rust comment syntax; the same leading `//!` run in a .ts file is not
        # a module doc, and checks 1 and 2 still scan it.
        over = doc(BUDGET + 5)
        self.assertEqual(self.run_main({"web/src/app.ts": over}), 0)

    def test_a_nested_checkout_is_not_repo_content(self):
        # An agent worktree is a full second copy of the tree, and a stale one by design. Every
        # finding in it names a path that reads as real and is about to be discarded.
        nested = ".claude/worktrees/agent-abc123/crates/reuben-core/src/lib.rs"
        self.assertEqual(self.run_main({nested: "//! see rules: no-such-topic\n"}), 0)

    def test_the_rest_of_dot_claude_is_still_scanned(self):
        # The converse, and the reason the skip is a root-anchored pair rather than all of
        # `.claude`: hooks, skills and settings are tracked source, governed like any other file.
        skill = ".claude/skills/control-surface/emit.py"
        self.assertEqual(self.run_main({skill: "# see rules: no-such-topic\n"}), 1)

    def test_a_worktrees_dir_elsewhere_is_not_a_nested_checkout(self):
        # The pair is anchored at the root: `worktrees` is not a reserved name further down, and a
        # source dir that happens to carry it stays in reach.
        self.assertEqual(self.run_main({"crates/worktrees/src/lib.rs": "//! see rules: nope\n"}), 1)


if __name__ == "__main__":
    unittest.main()

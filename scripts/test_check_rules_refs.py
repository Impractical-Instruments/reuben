#!/usr/bin/env python3
"""Unit tests for check_rules_refs — the ADR/pointer reference-linter and the module-doc guard.

Fixture trees are built with tempfile; the guard is imported as a bare module (tests run from
`scripts/`, mirroring the sibling link-guard tests). Each test asserts the exact problem count so a
regression that over- or under-reports is caught, not just pass/fail.

`module_doc_problems` is exercised directly on source text — no tree needed — and `main` is
exercised through a tree, since which paths check 3 reaches is the part that can regress.
"""
from __future__ import annotations
import os
import shutil
import subprocess
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


class CommentRefGuardJs(unittest.TestCase):
    """Check 4 in the JS lane: every comment form is in reach, because nothing in this repo
    generates prose out of a JS comment (no tsconfig, no checkJs, no typedoc/jsdoc; the tool
    schemas a model reads are a generated artifact built on the Rust side)."""

    def problems(self, text: str) -> list[str]:
        return check_rules_refs.comment_ref_problems("a.mjs", text + "\n", "curly")

    def test_a_line_comment_citing_an_issue_is_flagged(self):
        self.assertEqual(len(self.problems("// the compactHistory obituary (#107)")), 1)

    def test_the_cross_repo_spelling_is_flagged(self):
        problems = self.problems("  // source addressing landed in reuben-web#239")
        self.assertEqual(len(problems), 1)
        self.assertIn("reuben-web#239", problems[0])

    def test_a_triple_slash_is_in_reach_here(self):
        # The `///` carve-out is Rust's, and it is about generation, not about slashes: there a doc
        # comment becomes an advertised description. In JS `///` becomes nothing.
        self.assertEqual(len(self.problems("/// retired with #604")), 1)

    def test_a_block_comment_is_in_reach(self):
        # The decision this pins: JSDoc is not an advertised description HERE — nothing reads it —
        # so a block comment is only a comment, and the corpus uses it to argue.
        self.assertEqual(len(self.problems("/* superseded by #604 */")), 1)
        self.assertEqual(len(self.problems("/** @returns the shape #604 froze */")), 1)
        self.assertEqual(len(self.problems("/**\n * superseded by #604\n */")), 1)

    def test_a_block_comment_is_reached_the_same_way_with_or_without_a_url(self):
        # The regression that exposed the old carve-out as an accident: a block comment was
        # invisible UNLESS it happened to contain a `//`, at which point the URL — not the block —
        # was what the scanner saw. Both forms must now report identically.
        plain = self.problems("/**\n * a b #264 c\n */")
        urly = self.problems("/**\n * a https://ex.com/x/#264 b\n */")
        self.assertEqual(len(plain), 1)
        self.assertEqual(len(urly), 1)
        self.assertEqual(plain[0].split(":")[1], urly[0].split(":")[1])   # same line reported

    def test_a_string_literal_is_not_a_comment(self):
        # All three delimiters, because a guard that reds on a URL in a single-quoted string makes
        # ordinary
        # code unwritable and lands on somebody who has no idea why.
        self.assertEqual(self.problems('const s = "regression for #604";'), [])
        self.assertEqual(self.problems("const s = 'https://example.com/guide#604';"), [])
        self.assertEqual(self.problems("const s = `https://example.com/guide#604`;"), [])

    def test_a_comment_behind_a_string_is_still_reached(self):
        # The converse: skipping literals must not skip the comment behind one.
        self.assertEqual(len(self.problems("const s = 'x'; // fixed in #604")), 1)

    def test_a_template_literal_spanning_lines_is_not_prose(self):
        # A backtick is the one JS delimiter that survives a line break, so its continuation lines
        # are code — including any `//` in a URL on them.
        self.assertEqual(self.problems("const t = `line1\nhttps://x/#264`;"), [])


class CommentRefGuardRustBlocks(unittest.TestCase):
    """The other half of the same decision: in Rust a block comment CAN be a doc comment, and a doc
    comment can be generated into an advertised description, so it stays out of reach."""

    def problems(self, text: str) -> list[str]:
        return check_rules_refs.comment_ref_problems("a.rs", text + "\n", "rust")

    def test_a_rust_block_doc_comment_is_out_of_reach(self):
        self.assertEqual(self.problems("/** retired with #604 */"), [])
        self.assertEqual(self.problems("/*! module-level, retired with #604 */"), [])

    def test_a_rust_line_comment_is_in_reach(self):
        self.assertEqual(len(self.problems("// fixed in #604")), 1)

    def test_a_lifetime_does_not_swallow_the_comment(self):
        # `'` is a lifetime in Rust, which is why this lane tracks `"` only — treating it as a
        # string opener would hide every comment behind one.
        self.assertEqual(len(self.problems("fn f<'a>(x: &'a str) {} // fixed in #604")), 1)


class CommentRefGuardCss(unittest.TestCase):
    """Check 4 in the CSS lane. `/* … */` is the only comment form CSS has and nothing generates
    prose out of one, so the lane is in reach whole — with one lane fact the JS family does not
    share: `//` opens nothing here."""

    def problems(self, text: str) -> list[str]:
        return check_rules_refs.comment_ref_problems("a.css", text + "\n", "css")

    def test_a_block_comment_citing_an_issue_is_flagged(self):
        self.assertEqual(len(self.problems("/* the dismissible banner (#228) */")), 1)

    def test_a_block_comment_spanning_lines_is_flagged(self):
        self.assertEqual(len(self.problems("/* the co-presence spine\n   landed in #355 */")), 1)

    def test_the_cross_repo_spelling_is_flagged(self):
        problems = self.problems("/* reuben-web#239 moved this */")
        self.assertEqual(len(problems), 1)
        self.assertIn("reuben-web#239", problems[0])

    def test_a_double_slash_opens_no_comment(self):
        # The lane fact CSS does not share with JS: `//` is the authority slashes of a URL, never a
        # comment opener. Reading it as one turns the tail of every URL into prose.
        self.assertEqual(self.problems("  background: url(https://cdn.example.com/a#264);"), [])

    def test_a_protocol_relative_url_opens_no_comment(self):
        self.assertEqual(self.problems("  background: url(//cdn.example.com/a#264);"), [])

    def test_a_quoted_url_is_not_a_comment(self):
        self.assertEqual(self.problems("  background: url('https://x/a#264');"), [])
        self.assertEqual(self.problems('  background: url("https://x/a#264");'), [])

    def test_a_comment_behind_a_string_is_still_reached(self):
        self.assertEqual(len(self.problems("  content: 'x'; /* retired in #604 */")), 1)

    def test_a_stray_backtick_does_not_swallow_the_rest_of_the_file(self):
        # CSS has no template literal, so a backtick is not a delimiter here. The JS lane must track
        # one — it is the single delimiter that survives a line break — and carrying that over would
        # put every comment below a stray backtick out of reach.
        self.assertEqual(len(self.problems("  --x: `;\n/* retired in #604 */")), 1)

    def test_a_hex_colour_in_a_declaration_is_not_prose(self):
        # A colour is a declaration, and this check reads comment prose only.
        self.assertEqual(self.problems("  --moss: #719; color: #0088;"), [])

    def test_a_short_hex_colour_in_a_comment_is_flagged_deliberately(self):
        # The collision this lane brings and no other does: an all-digit three- or four-digit colour
        # is spelled exactly like an issue citation, and nothing around it decides which. The strict
        # reading is the chosen one — a colour restated in a comment is restatement anyway.
        self.assertEqual(len(self.problems("/* was #333 before */")), 1)

    def test_the_colour_collision_is_named_in_the_message(self):
        # Strictness with no explanation is what teaches people to route around a guard. Neither the
        # docstring nor the rules doc is the surface an author hits — the error string is.
        problems = self.problems("/* was #333 before */")
        self.assertIn("hex COLOUR", problems[0])

    def test_a_commented_out_declaration_gets_the_hint(self):
        # The most ordinary comment a stylesheet carries, landing on the grey ramp.
        problems = self.problems("/* background: #444; */")
        self.assertEqual(len(problems), 1)
        self.assertIn("hex COLOUR", problems[0])

    def test_a_cross_repo_citation_gets_no_colour_hint(self):
        # The `<repo>#<nnn>` spelling cannot be a colour, so the hint would be noise.
        problems = self.problems("/* reuben-web#333 moved this */")
        self.assertEqual(len(problems), 1)
        self.assertNotIn("hex COLOUR", problems[0])

    def test_a_citation_too_short_to_be_a_colour_gets_no_hint(self):
        # CSS has no two-digit hex colour, so this one is unambiguous and needs no caveat.
        problems = self.problems("/* the banner (#90) */")
        self.assertEqual(len(problems), 1)
        self.assertNotIn("hex COLOUR", problems[0])

    def test_no_other_lane_gets_the_colour_hint(self):
        # One branch, on the lane: a JS comment has no colour to be mistaken for.
        problems = check_rules_refs.comment_ref_problems("a.mjs", "// see #333\n", "curly")
        self.assertEqual(len(problems), 1)
        self.assertNotIn("hex COLOUR", problems[0])

    def test_a_rules_pointer_is_clean(self):
        self.assertEqual(self.problems("/* see rules: code-as-grounding */"), [])


class CommentRefGuardHash(unittest.TestCase):
    """Check 4 in the `#` lanes — shell, YAML, TOML, Python. Nothing here is generated into prose
    anyone reads, so the lane is in reach whole."""

    def problems(self, text: str, rel: str = "a.yml") -> list[str]:
        return check_rules_refs.comment_ref_problems(rel, text + "\n", "hash")

    def test_a_hash_comment_citing_an_issue_is_flagged(self):
        self.assertEqual(len(self.problems("  # fixed in reuben#604, see there")), 1)

    def test_a_citation_that_opens_the_comment_is_found(self):
        # Regression: where the citation's own hash IS the comment opener, the body starts one
        # character too late and the citation arrives as a bare number. Reading the body alone is
        # blind to it, and blind only in the lanes the ban was just widened to reach.
        self.assertEqual(len(self.problems("#604 fixed this")), 1)

    def test_every_citation_on_the_line_is_found(self):
        # The same bug's common shape: the first of two goes missing and the report reads as if
        # there were one, which is worse than missing both.
        self.assertEqual(len(self.problems("access. #603 and #604 removed that")), 2)

    def test_a_shebang_is_not_an_issue_ref(self):
        self.assertEqual(self.problems("#!/usr/bin/env bash"), [])

    def test_a_quoted_scalar_is_not_a_comment(self):
        self.assertEqual(self.problems("  note: 'regression for #604'"), [])
        self.assertEqual(self.problems("  - run: echo 'a # b #604'"), [])

    def test_a_bare_url_fragment_is_not_a_comment(self):
        # YAML and shell both open a comment at `#` only after whitespace, so a fragment inside a
        # bare URL is data. Without this the guard reds on an ordinary `curl`.
        self.assertEqual(self.problems("      - run: curl -sS https://example.com/spec#604"), [])

    def test_an_apostrophe_in_a_plain_scalar_does_not_hide_the_comment(self):
        # The blind spot that came with the same defect: a quote only opens a scalar at a TOKEN
        # start, so `Charlie's` is literal and the comment after it is still prose.
        self.assertEqual(len(self.problems("  - name: Charlie's step  # retired in #604")), 1)

    def test_a_single_digit_reference_is_not_an_issue_ref(self):
        self.assertEqual(self.problems("# step #3 of the handshake"), [])


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
                "a.py", text + "\n", lambda cross, slug: slug == "code-as-grounding", "hash"
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


class LaneKeyAndSuffixClass(unittest.TestCase):
    """Check 7: reach is default-on, so every lane key in the tree is classified — readable,
    deliberately out of reach, or not a prose surface — and one in no bucket is reported.

    This is the check that makes default-on true of the CODE rather than only of the docstring.
    """

    def test_a_dotenv_is_keyed_by_format_not_by_environment(self):
        # `.env.production` has suffix `.production`, which names an environment and matches no row.
        self.assertEqual(check_rules_refs.lane_key(".env.production"), ".env")
        self.assertEqual(check_rules_refs.lane_key(".env"), ".env")
        self.assertEqual(check_rules_refs.lane_key("a.css"), ".css")

    def test_a_dot_prefixed_name_is_the_format(self):
        # The dotenv argument, stated once rather than once per file: these carry no suffix at all,
        # and `#` has opened a comment in every one of them the whole time.
        for name in (".gitignore", ".gitattributes", ".gitmodules", ".ignore"):
            with self.subTest(name=name):
                self.assertEqual(check_rules_refs.lane_key(name), name)
                self.assertIn(name, check_rules_refs.CODE_EXTS)

    def test_a_shebang_is_a_lane_declaration(self):
        # An extensionless executable declares its format on line one, and `#!/bin/sh` is as good a
        # declaration as a suffix.
        self.assertEqual(check_rules_refs.lane_key("pre-commit", "#!/bin/sh"), "#!")
        self.assertEqual(check_rules_refs.lane_key("gate", "#!/usr/bin/env bash"), "#!")
        self.assertEqual(check_rules_refs.lane_key("gen", "#!/usr/bin/env python3"), "#!")

    def test_an_unknown_interpreter_is_reported_rather_than_guessed(self):
        # `#` opens a comment in a shell script and declares a private field in a JS one. An
        # interpreter nobody has checked keys as ITSELF and lands in this check, rather than being
        # read with an opener chosen on a hunch.
        key = check_rules_refs.lane_key("tool", "#!/usr/bin/env node")
        self.assertEqual(key, "#!node")
        self.assertEqual(len(check_rules_refs.suffix_class_problems({key: "tool"})), 1)

    def test_a_file_declaring_no_format_anywhere_is_still_classified(self):
        # LICENSE, NOTICE. The key is "", which is a key like any other and carries its own reason,
        # rather than dropping out of the census — which is how four `.gitignore`s stayed invisible
        # while this check reported every lane accounted for.
        self.assertEqual(check_rules_refs.lane_key("LICENSE"), "")
        self.assertEqual(check_rules_refs.lane_key("LICENSE", "GNU AFFERO GENERAL PUBLIC LICENSE"), "")
        self.assertEqual(check_rules_refs.suffix_class_problems({"": "LICENSE"}), [])

    def test_the_lane_key_is_casefolded(self):
        # Latent in both trees, and the advice it produced was nonsense: an unclassified `.JPG` was
        # told to earn itself a comment-opener row.
        self.assertEqual(check_rules_refs.lane_key("UPPER.CSS"), ".css")
        self.assertEqual(check_rules_refs.lane_key("file.RS"), ".rs")
        self.assertEqual(check_rules_refs.lane_key("README.MD"), ".md")
        self.assertEqual(check_rules_refs.lane_key("photo.JPG"), ".jpg")

    def test_a_readable_lane_is_clean(self):
        self.assertEqual(check_rules_refs.suffix_class_problems({".rs": "a.rs"}), [])

    def test_an_out_of_reach_lane_is_clean(self):
        self.assertEqual(check_rules_refs.suffix_class_problems({".md": "a.md"}), [])

    def test_a_non_source_lane_is_clean(self):
        self.assertEqual(check_rules_refs.suffix_class_problems({".png": "a.png"}), [])

    def test_an_unclassified_lane_is_reported(self):
        problems = check_rules_refs.suffix_class_problems({".scss": "web/a.scss"})
        self.assertEqual(len(problems), 1)
        self.assertIn("unclassified lane `.scss`", problems[0])
        self.assertTrue(problems[0].startswith("web/a.scss:1:"))

    def test_both_exclusion_buckets_carry_reasons(self):
        # The entry IS the reason, in BOTH buckets. If only one demanded a sentence, the other would
        # be the cheap door out of a red here, and the silent allow-list would be back under a third
        # name. Shared reasons are fine — the argument is per KIND — but a bare token is not.
        for bucket in (check_rules_refs.OUT_OF_REACH_EXTS, check_rules_refs.NOT_SOURCE_EXTS):
            for ext, reason in bucket.items():
                with self.subTest(ext=ext):
                    self.assertGreaterEqual(len(reason.split()), 10)


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
        # Check 3 reads Rust comment syntax; the same leading `//!` run in a .ts file is not a
        # module doc. Checks 1, 2 and 4 still scan it.
        over = doc(BUDGET + 5)
        self.assertEqual(self.run_main({"web/src/app.ts": over}), 0)

    def test_a_js_file_citing_an_issue_is_flagged(self):
        # The reach the ban was missing: a `.mjs` line comment was checked for pointer grammar but
        # never for provenance, so an issue number in one was unpoliced.
        self.assertEqual(self.run_main({"js/agent-host.mjs": "// see #107\n"}), 1)

    def test_a_js_block_comment_is_flagged(self):
        # The scope decision at the tree level. JSDoc is not an advertised description in this
        # repo — nothing reads it — so a block comment is prose like any other.
        self.assertEqual(self.run_main({"js/agent-host.mjs": "/** see #107 */\n"}), 1)

    def test_a_rust_doc_comment_is_still_not_flagged(self):
        # The carve-out survives the widening, in the one lane whose argument it is — and it is
        # about GENERATION, so it covers the block form there too.
        self.assertEqual(self.run_main({"src/lib.rs": "/// see #107\n"}), 0)
        self.assertEqual(self.run_main({"src/lib.rs": "/** see #107 */\n"}), 0)

    def test_a_pointer_inside_a_js_block_comment_is_validated(self):
        # Checks 2/5 read the same scan, so a topic named inside a block comment resolves or is
        # reported. 72 pointers in this corpus lived in that gap.
        self.assertEqual(self.run_main({"js/a.mjs": "/**\n * see rules: no-such-topic\n */\n"}), 1)
        self.assertEqual(
            self.run_main({"js/a.mjs": "/**\n * see rules: code-as-grounding\n */\n"}), 0)

    def test_a_hash_lane_citing_an_issue_is_flagged(self):
        # Workflow, shell and manifest prose: no advertised description anywhere, so no carve-out.
        for rel in (".github/workflows/ci.yml", "scripts/gate.sh", "Cargo.toml"):
            with self.subTest(rel=rel):
                self.assertEqual(self.run_main({rel: "# see #107\n"}), 1)

    def test_a_css_file_citing_an_issue_is_flagged(self):
        # The lane the guard could not READ: `.css` had no row in CODE_EXTS, so a stylesheet comment
        # was unpoliced rather than exempt — the failure mode that decided the lane list's shape.
        self.assertEqual(self.run_main({"web/src/app.css": "/* the banner (#228) */\n"}), 1)

    def test_a_css_url_is_not_a_css_comment(self):
        self.assertEqual(
            self.run_main({"web/src/app.css": "a { background: url(https://x/y#264); }\n"}), 0)

    def test_a_pointer_inside_a_css_comment_is_validated(self):
        self.assertEqual(self.run_main({"web/src/a.css": "/* see rules: no-such-topic */\n"}), 1)
        self.assertEqual(
            self.run_main({"web/src/a.css": "/* see rules: code-as-grounding */\n"}), 0)

    def test_an_unclassified_lane_reds_the_tree(self):
        # The mechanism, end to end: a lane the guard has never met is a sentence, not a shrug.
        self.assertEqual(self.run_main({"web/a.scss": ".x { color: red; }\n"}), 1)

    def test_a_dotenv_is_read_as_a_hash_lane(self):
        # Found by check 7 rather than by anyone noticing, and carrying a real citation when it was.
        self.assertEqual(self.run_main({"web/.env.production": "# baked in by #107\n"}), 1)

    def test_html_is_classified_out_rather_than_missing(self):
        # The honest state: three comment regimes in one file, so it is not readable with a single
        # opener and its comments are UNPOLICED — which is not the same sentence as exempt. The
        # citation below is deliberately NOT flagged, and this test is where that gap is written down.
        self.assertEqual(self.run_main({"web/index.html": "<!-- the shell (#107) -->\n"}), 0)

    def test_a_directory_named_engine_is_not_the_submodule(self):
        # The skip is about BEING the submodule, which only the root position says. As a bare
        # component it also swallowed `crates/reuben-api/src/engine/`, first-party modules that
        # went unread under this repo's own CI.
        self.assertEqual(
            self.run_main({"crates/reuben-api/src/engine/mod.rs": "//! see rules: nope\n"}), 1)

    def test_the_submodule_at_the_root_is_still_skipped(self):
        # The converse: a run from the consuming repo's root must not read the submodule's tree.
        self.assertEqual(
            self.run_main({"engine/crates/reuben-core/src/lib.rs": "//! see rules: nope\n"}), 0)

    def test_a_gitignore_citing_an_issue_is_flagged(self):
        # The blind spot check 7 could not see, because it had no key for these to be unclassified
        # UNDER: four of these were carrying citations across the two repos.
        self.assertEqual(self.run_main({".gitignore": "# generated by #107\nnode_modules/\n"}), 1)

    def test_a_githook_citing_an_issue_is_flagged(self):
        # No suffix and no dot — the format is on line one.
        self.assertEqual(
            self.run_main({"scripts/hooks/pre-commit": "#!/bin/sh\n# run from here since #107\n"}), 1)

    def test_a_file_declaring_no_format_is_neither_scanned_nor_reported(self):
        # LICENSE states no comment syntax, so it has no comments for the ban to reach — and that
        # is a written classification now, not an omission.
        self.assertEqual(self.run_main({"LICENSE": "GNU AFFERO GENERAL PUBLIC LICENSE\n#107\n"}), 0)

    def test_a_skill_exempt_from_the_adr_rule_is_still_classified(self):
        # SKILL_ALLOWLIST exempts a skill from the ADR-token rule and from nothing else. A `.scss`
        # under one is still an unclassified lane, so classification runs before that filter.
        self.assertEqual(
            self.run_main({".claude/skills/absorb-adrs/theme.scss": ".x { color: red; }\n"}), 1)

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


class RepoContentNotFilesystem(unittest.TestCase):
    """What the guard walks is the repository, not the disk under it.

    `rglob` made the guard un-runnable locally the moment anybody ran the tests: a Playwright run
    leaves `test-results/**/video.webm`, and check 7 would report an unclassified `.webm` and advise
    giving a video file a comment opener. CI never saw it, because a clean checkout has no junk.
    """

    def build_repo(self, files: dict[str, str]) -> Path:
        tmp = tempfile.mkdtemp()
        root = Path(tmp)
        self.addCleanup(shutil.rmtree, tmp, True)
        build(root, files)
        env = {**os.environ, "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_SYSTEM": "/dev/null"}
        subprocess.run(["git", "init", "-q", str(root)], check=True, env=env)
        subprocess.run(["git", "-C", str(root), "add", "-A"], check=True, env=env)
        return root

    def test_an_ignored_artifact_is_not_repo_content(self):
        root = self.build_repo({".gitignore": "test-results/\n",
                                "test-results/run/video.webm": "not really a video\n"})
        self.assertEqual(check_rules_refs.main(str(root)), 0)

    def test_the_same_artifact_unignored_is_repo_content(self):
        # The converse, so the skip is provably git's opinion and not a hardcoded directory name:
        # nothing about `test-results` is special, only about it being disclaimed.
        root = self.build_repo({".gitignore": "nothing/\n",
                                "test-results/run/video.webm": "not really a video\n"})
        problems = check_rules_refs.main(str(root))
        self.assertEqual(problems, 1)

    def test_an_untracked_but_unignored_file_is_still_scanned(self):
        # `--others --exclude-standard`, not `--cached`: a file just written and not yet added is
        # repo content, and a pre-commit run has to see it.
        root = self.build_repo({".gitignore": "junk/\n"})
        (root / "src").mkdir(parents=True, exist_ok=True)
        (root / "src" / "new.rs").write_text("// fixed in #107\n", encoding="utf-8")
        self.assertEqual(check_rules_refs.main(str(root)), 1)

    def test_a_tree_that_is_not_a_repo_falls_back_to_the_filesystem(self):
        # The fixture trees every other test in this file uses are exactly that case.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            build(root, {"src/lib.rs": "// fixed in #107\n"})
            self.assertIsNone(check_rules_refs.repo_files(root))
            self.assertEqual(check_rules_refs.main(str(root)), 1)


if __name__ == "__main__":
    unittest.main()

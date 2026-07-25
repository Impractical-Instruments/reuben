#!/usr/bin/env python3
"""Unit tests for extract_doc_claims — the doc-surface claim ledger.

Fixture trees are built with tempfile; the extractor is imported as a bare module (tests run from
`scripts/`, mirroring the engine's skill-test idiom). Tests assert the *classification* of a claim
— decidable-and-failing vs routed-for-review — because that split is what the layer exists to get
right; a check that merely counted problems would not notice it inverting.
"""
from __future__ import annotations
import tempfile
import unittest
from pathlib import Path

import extract_doc_claims as ex


def build(root: Path, files: dict[str, str]):
    """Write {relative path: contents} under root, creating parents."""
    for rel, body in files.items():
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(body, encoding="utf-8")


class ClaimLedgerTest(unittest.TestCase):
    def _claims(self, files, since=None):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, files)
            return ex.collect(str(root), since)

    def _one(self, files, kind):
        got = [c for c in self._claims(files) if c.kind == kind]
        self.assertEqual(len(got), 1, f"expected exactly one {kind} claim, got {got}")
        return got[0]

    def test_empty_tree_is_green(self):
        self.assertEqual(self._claims({}), [])

    # --- scope ---

    def test_ungoverned_docs_are_not_scanned(self):
        # A research note naming a moved file is history, not a false claim.
        claims = self._claims({"docs/research/notes.md": "See `nowhere/gone.rs`.\n"})
        self.assertEqual(claims, [])

    def test_symlinked_root_doc_is_not_double_counted(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {"AGENTS.md": "See `nowhere/gone.rs`.\n"})
            (root / "CLAUDE.md").symlink_to("AGENTS.md")
            paths = [c.file for c in ex.collect(str(root)) if c.kind == "path"]
            self.assertEqual(paths, ["AGENTS.md"])

    # --- path claims ---

    def test_path_with_slash_that_resolves_exactly(self):
        claim = self._one(
            {"AGENTS.md": "Enter at `crates/core/src/plan.rs`.\n",
             "crates/core/src/plan.rs": "fn main() {}\n"}, "path")
        self.assertEqual((claim.decidable, claim.status), (True, "ok"))

    def test_path_resolves_by_suffix(self):
        # Docs write the short form for a file that lives deeper; that is not a defect.
        claim = self._one(
            {"docs/agents/guide.md": "Enter at `format/normalize.rs`.\n",
             "crates/core/src/format/normalize.rs": "fn main() {}\n"}, "path")
        self.assertEqual((claim.decidable, claim.status), (True, "ok"))

    def test_entry_doc_path_must_resolve_in_full(self):
        # AGENTS.md is navigation: an agent opens what it names, so a suffix that resolves for a
        # reader but not for `Read` is a defect there even though it passes anywhere else.
        claim = self._one(
            {"AGENTS.md": "Enter at `reuben-core/src/plan.rs`.\n",
             "crates/reuben-core/src/plan.rs": "fn main() {}\n"}, "path")
        self.assertEqual((claim.decidable, claim.status), (True, "unresolved"))
        self.assertIn("openable", claim.note)

    def test_entry_doc_full_path_passes(self):
        claim = self._one(
            {"AGENTS.md": "Enter at `crates/reuben-core/src/plan.rs`.\n",
             "crates/reuben-core/src/plan.rs": "fn main() {}\n"}, "path")
        self.assertEqual((claim.decidable, claim.status), (True, "ok"))

    def test_path_with_slash_that_resolves_nowhere_fails(self):
        claim = self._one({"AGENTS.md": "Enter at `crates/core/src/gone.rs`.\n"}, "path")
        self.assertEqual((claim.decidable, claim.status), (True, "unresolved"))

    def test_bare_filename_matching_nothing_is_routed_not_failed(self):
        # `a.json` in an example is not a claim about the repo, and deciding that is judgment.
        claim = self._one({"AGENTS.md": "Save it as `a.json`.\n"}, "path")
        self.assertEqual((claim.decidable, claim.status), (False, "needs-review"))

    def test_dot_slash_prefix_is_read_as_a_bare_filename(self):
        claim = self._one({"AGENTS.md": "Save it as `./a.json`.\n"}, "path")
        self.assertEqual((claim.decidable, claim.status), (False, "needs-review"))

    def test_path_inside_a_fenced_block_is_not_a_claim(self):
        self.assertEqual(
            [c for c in self._claims(
                {"AGENTS.md": "```sh\ncat `crates/core/gone.rs`\n```\n"}) if c.kind == "path"],
            [])

    # --- identifier claims ---

    def test_identifier_present_in_source(self):
        claim = self._one(
            {"docs/agents/guide.md": "Preallocate `RenderScratch`.\n",
             "crates/core/src/render.rs": "pub struct RenderScratch;\n"}, "identifier")
        self.assertEqual((claim.decidable, claim.status), (True, "ok"))

    def test_identifier_absent_from_a_now_state_doc_fails(self):
        claim = self._one({"docs/agents/guide.md": "Preallocate `RenderContext`.\n"}, "identifier")
        self.assertEqual((claim.decidable, claim.status), (True, "unresolved"))

    def test_identifier_absent_from_a_rationale_is_routed_not_failed(self):
        # A rationale names what it REJECTED. Requiring those to exist inverts its meaning.
        claim = self._one(
            {"docs/rules/x.md": "# X\n\n> s\n\n## Rules\n\n<a id=\"r\"></a>\n### R.\n\n"
                                "[why](rationale/x/r.md)\n",
             "docs/rules/rationale/x/r.md": "The names are `In`/`Out`, not `InPort`.\n"},
            "identifier")
        self.assertEqual((claim.decidable, claim.status), (False, "needs-review"))

    def test_qualified_identifier_yields_its_segments(self):
        claim = self._one({"docs/agents/guide.md": "Call `RenderContext::new` early.\n"},
                          "identifier")
        self.assertEqual((claim.text, claim.status), ("RenderContext", "unresolved"))

    def test_known_external_identifier_passes_with_its_reason(self):
        claim = self._one({"docs/agents/guide.md": "The host owns `Float32Array`.\n"}, "identifier")
        self.assertEqual((claim.decidable, claim.status), (True, "ok"))
        self.assertIn("browser", claim.note)

    def test_short_names_are_not_read_as_identifiers(self):
        # `f32`, `ok`, one-hump `Descriptor` — deliberately below the claim threshold.
        self.assertEqual(
            [c for c in self._claims({"docs/agents/g.md": "It is `f32`, `ok`, `Descriptor`.\n"})
             if c.kind == "identifier"],
            [])

    def test_extractor_source_is_not_its_own_evidence(self):
        # SELF_EXCLUDED: a name this script mentions as an example must not vouch for a doc.
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            build(root, {"docs/agents/g.md": "Preallocate `MadeUpType`.\n",
                         "scripts/extract_doc_claims.py": "# MadeUpType is an example\n"})
            claim = [c for c in ex.collect(str(root)) if c.kind == "identifier"][0]
            self.assertEqual(claim.status, "unresolved")

    # --- guard claims ---

    def test_guard_line_naming_a_real_test_passes(self):
        claim = self._one(
            {"docs/rules/x.md": "# X\n\n> s\n\n## Rules\n\n<a id=\"r\"></a>\n### R.\n\n"
                                "Guarded by: tests/wire.rs::schemas_match\n\n[why](rationale/x/r.md)\n",
             "docs/rules/rationale/x/r.md": "why\n",
             "crates/core/tests/wire.rs": "#[test]\nfn schemas_match() {}\n"}, "guard")
        self.assertEqual((claim.decidable, claim.status), (True, "ok"))

    def test_guard_line_naming_a_missing_test_fails(self):
        claim = self._one(
            {"docs/rules/x.md": "# X\n\n> s\n\n## Rules\n\n<a id=\"r\"></a>\n### R.\n\n"
                                "Guarded by: tests/wire.rs::nope\n\n[why](rationale/x/r.md)\n",
             "docs/rules/rationale/x/r.md": "why\n",
             "crates/core/tests/wire.rs": "#[test]\nfn schemas_match() {}\n"}, "guard")
        self.assertEqual((claim.decidable, claim.status), (True, "unresolved"))

    # --- roster counts ---

    def test_roster_count_fails_the_build(self):
        claim = self._one({"docs/agents/g.md": "The browser binds the same eight contracts.\n"},
                          "roster-count")
        self.assertEqual((claim.decidable, claim.status), (True, "unresolved"))

    def test_roster_count_is_caught_through_backticks(self):
        # Docs write "across 27 `tools`"; the markup must not hide the number from the ban.
        claim = self._one({"docs/agents/g.md": "Across 27 `tools` the schemas agree.\n"},
                          "roster-count")
        self.assertEqual(claim.status, "unresolved")

    def test_roster_count_survives_one_adjective(self):
        claim = self._one({"docs/agents/g.md": "It advertises nineteen document verbs.\n"},
                          "roster-count")
        self.assertEqual(claim.status, "unresolved")

    def test_a_pair_is_architecture_not_a_roster(self):
        # "the two verbs over typed handles" names a design, not a roster that grows.
        self.assertEqual(
            [c for c in self._claims({"docs/agents/g.md": "Two verbs read and write: `io` does it.\n"})
             if c.kind == "roster-count"],
            [])

    def test_roster_count_inside_a_fenced_block_is_not_a_claim(self):
        self.assertEqual(
            [c for c in self._claims({"docs/agents/g.md": "```sh\n# 27 tools listed\n```\n"})
             if c.kind == "roster-count"],
            [])

    # --- routed claims ---

    def test_count_next_to_code_is_routed(self):
        claim = self._one({"docs/agents/g.md": "It ships the same eight `resources` today.\n"},
                          "count")
        self.assertEqual((claim.decidable, claim.status), (False, "needs-review"))

    def test_count_with_no_code_on_the_line_is_not_extracted(self):
        # "two devices" is architecture; it will never be checkable and must not fill the worklist.
        self.assertEqual(
            [c for c in self._claims({"docs/agents/g.md": "There are two devices.\n"})
             if c.kind == "count"],
            [])

    def test_single_sourcing_is_keyed_by_rule_not_by_line(self):
        # The slug appears on its anchor AND in its [why] link; that is one claim, not three.
        body = ("# X\n\n> s\n\n## Rules\n\n<a id=\"single-source-contract\"></a>\n"
                "### It is single-sourced from one declaration.\n\n"
                "[why](rationale/x/single-source-contract.md)\n")
        claims = self._claims({"docs/rules/x.md": body,
                               "docs/rules/rationale/x/single-source-contract.md": "why\n"})
        ss = [c for c in claims if c.kind == "single-sourcing"]
        self.assertEqual(len(ss), 1)
        self.assertEqual(ss[0].text, "#single-source-contract")

    def test_single_sourcing_in_a_rationale_is_not_extracted(self):
        # A rationale re-argues the claim; only the rule is normative and owes a guard.
        claims = self._claims(
            {"docs/rules/x.md": "# X\n\n> s\n\n## Rules\n\n<a id=\"r\"></a>\n### R.\n\n"
                                "[why](rationale/x/r.md)\n",
             "docs/rules/rationale/x/r.md": "It is single-sourced, generated from one source.\n"})
        self.assertEqual([c for c in claims if c.kind == "single-sourcing"], [])


if __name__ == "__main__":
    unittest.main()

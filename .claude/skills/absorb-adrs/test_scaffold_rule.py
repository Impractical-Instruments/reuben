#!/usr/bin/env python3
"""Tests for scaffold_rule.py — the absorb-adrs rule<->rationale scaffolder.

The real proof is mechanical: scaffold into a throwaway rules tree seeded with the repo's actual
S01 templates + README, then run the REAL S01 guards (`check_rules_links.py`,
`check_rules_derive.py`) over the output and assert they go green. If the scaffold ever drifts from
the invariant the guards enforce, this reds in CI's `check` job (which runs `.claude/skills/*/test_*.py`).
"""
from __future__ import annotations
import shutil, subprocess, sys, tempfile, unittest
from pathlib import Path

import scaffold_rule

REPO = Path(__file__).resolve().parents[3]  # .claude/skills/absorb-adrs/ -> repo root
LINKS = REPO / "scripts" / "check_rules_links.py"
DERIVE = REPO / "scripts" / "check_rules_derive.py"


def run_guard(script: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, str(script), *args], capture_output=True, text=True)


class ScaffoldRuleTest(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        rules = self.tmp / "docs" / "rules"
        rules.mkdir(parents=True)
        shutil.copytree(REPO / "docs" / "rules" / "_templates", rules / "_templates")
        shutil.copy(REPO / "docs" / "rules" / "README.md", rules / "README.md")

    def scaffold(self, **kw) -> int:
        argv = []
        for k, v in kw.items():
            argv += [f"--{k}", v]
        argv += ["--root", str(self.tmp)]
        return scaffold_rule.main(argv)

    def assert_guards_green(self):
        # links guard must pass on the topic docs the scaffold produced
        links = run_guard(LINKS, str(self.tmp))
        self.assertEqual(links.returncode, 0, f"links guard failed:\n{links.stderr}")
        # derive: collate README from the topics (--write), then the CI backstop (--check) must pass
        self.assertEqual(run_guard(DERIVE, "--write", str(self.tmp)).returncode, 0)
        check = run_guard(DERIVE, "--check", str(self.tmp))
        self.assertEqual(check.returncode, 0, f"derive --check failed:\n{check.stderr}")

    def topic_text(self, topic="signal-time-dsp") -> str:
        return (self.tmp / "docs" / "rules" / f"{topic}.md").read_text()

    def rationale_path(self, topic="signal-time-dsp", rule="osc-only-core") -> Path:
        return self.tmp / "docs" / "rules" / "rationale" / topic / f"{rule}.md"

    # --- happy paths -------------------------------------------------------------------------
    def test_new_topic_is_guard_green(self):
        rc = self.scaffold(topic="signal-time-dsp", title="Signal / OSC / time / DSP",
                           summary="The OSC-only message model and DSP families.",
                           rule="osc-only-core",
                           heading="The core speaks only OSC-shaped Messages.")
        self.assertEqual(rc, 0)
        text = self.topic_text()
        self.assertIn('<a id="osc-only-core"></a>', text)
        self.assertIn("### The core speaks only OSC-shaped Messages.", text)
        self.assertIn("[why](rationale/signal-time-dsp/osc-only-core.md)", text)
        self.assertEqual(text.count("[why]"), 1)  # strictly one why per rule
        rat = self.rationale_path().read_text()
        self.assertIn("[Rule](../../signal-time-dsp.md#osc-only-core)", rat)
        self.assertIn("Distilled from:", rat)
        self.assertNotIn("<The condensed", rat)  # body placeholder was swapped for a TODO
        self.assert_guards_green()

    def test_provenance_from_flag_lands_in_rationale(self):
        self.scaffold(topic="signal-time-dsp", title="Signal / OSC / time / DSP",
                      summary="The OSC-only message model.", rule="osc-only-core",
                      heading="The core speaks only OSC-shaped Messages.",
                      **{"from": "ADR-0007"})
        self.assertIn("Distilled from: ADR-0007", self.rationale_path().read_text())

    def test_second_rule_appends_and_stays_green(self):
        self.scaffold(topic="signal-time-dsp", title="Signal / OSC / time / DSP",
                      summary="The OSC-only message model.", rule="osc-only-core",
                      heading="The core speaks only OSC-shaped Messages.")
        rc = self.scaffold(topic="signal-time-dsp", title="Signal / OSC / time / DSP",
                           summary="The OSC-only message model.", rule="global-clock-grooves",
                           heading="A global default Clock grooves the whole graph.")
        self.assertEqual(rc, 0)
        text = self.topic_text()
        self.assertEqual(text.count("<a id="), 2)
        self.assertEqual(text.count("[why]"), 2)
        # both rule blocks live before `## Terms`
        self.assertLess(text.index('id="osc-only-core"'), text.index("## Terms"))
        self.assertLess(text.index('id="global-clock-grooves"'), text.index("## Terms"))
        self.assert_guards_green()

    # --- refusals ----------------------------------------------------------------------------
    def test_refuses_to_clobber_rationale(self):
        self.scaffold(topic="signal-time-dsp", title="T", summary="s", rule="osc-only-core",
                      heading="H.")
        marker = "SENTINEL-DO-NOT-OVERWRITE"
        p = self.rationale_path()
        p.write_text(marker)
        rc = self.scaffold(topic="signal-time-dsp", title="T", summary="s", rule="osc-only-core",
                           heading="H2.")
        self.assertNotEqual(rc, 0)
        self.assertEqual(p.read_text(), marker)  # untouched

    def test_refuses_duplicate_rule_slug(self):
        self.scaffold(topic="signal-time-dsp", title="T", summary="s", rule="osc-only-core",
                      heading="H.")
        # remove the rationale so the clobber guard doesn't mask the duplicate-anchor guard
        self.rationale_path().unlink()
        rc = self.scaffold(topic="signal-time-dsp", title="T", summary="s", rule="osc-only-core",
                           heading="H again.")
        self.assertNotEqual(rc, 0)
        self.assertEqual(self.topic_text().count('id="osc-only-core"'), 1)  # not duplicated

    def test_rejects_non_kebab_slug(self):
        self.assertNotEqual(
            self.scaffold(topic="Bad Slug", title="T", summary="s", rule="ok", heading="H."), 0)
        self.assertNotEqual(
            self.scaffold(topic="ok-topic", title="T", summary="s", rule="Bad_Rule", heading="H."), 0)

    # --- render_rationale hardening (template-drift robustness) -------------------------------
    def test_render_rationale_collapses_multi_placeholder_body(self):
        # A drifted template whose reasoning body carries TWO `<...>` placeholders. The old
        # whole-text `<...>` regex replaced each independently -> two BODY_TODOs (mangled); the
        # structural body-scope collapses the entire region to exactly one. This reds against the
        # pre-fix logic and greens after it.
        tpl = ("# Why: <the rule, named or restated>\n\n"
               "[Rule](../../<topic>.md#<rule-slug>)\n\n"
               "<first reasoning placeholder>\n\n"
               "<second reasoning placeholder>\n\n"
               "Distilled from: ADR-00xx\n")
        out = scaffold_rule.render_rationale(tpl, "signal-time-dsp", "osc-only-core", "H.", "ADR-0007")
        self.assertEqual(out.count(scaffold_rule.BODY_TODO), 1)
        self.assertNotIn("<first reasoning", out)
        self.assertNotIn("<second reasoning", out)
        self.assertIn("[Rule](../../signal-time-dsp.md#osc-only-core)", out)
        self.assertIn("Distilled from: ADR-0007", out)

    def test_render_rationale_raises_on_missing_anchors(self):
        # Template drift that drops the structural anchors must fail loudly, not silently mis-render.
        with self.assertRaises(ValueError):
            scaffold_rule.render_rationale("# Why: x\n\nbody, no anchors\n", "t", "r", "H.", "ADR-1")

    def test_render_rationale_matches_real_template_shape(self):
        # Against the repo's actual template, the happy path yields exactly one BODY_TODO and no
        # surviving `<...>` field placeholder.
        tpl = (REPO / "docs" / "rules" / "_templates" / "rationale.md").read_text()
        out = scaffold_rule.render_rationale(tpl, "signal-time-dsp", "osc-only-core", "H.", "ADR-0007")
        self.assertEqual(out.count(scaffold_rule.BODY_TODO), 1)
        self.assertNotIn("<The condensed", out)
        self.assertNotIn("<the rule", out)
        self.assertNotIn("<topic>", out)


if __name__ == "__main__":
    unittest.main()

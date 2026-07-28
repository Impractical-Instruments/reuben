"""Tests for the dashboard's eval half — the one that actually reaches a human.

The charts and tables are best-effort rendering, but `eval_rewrite_note` makes a *claim* about what
a discontinuity means, and a wrong claim there is worse than no claim: it tells the reader to
discount a real regression, or invents a re-baseline that never happened. `eval-history.jsonl`
accumulates across trend branches and gains fields over time, so the ragged inputs below are the
ones it actually meets.

Run from this directory, the way `scripts/`'s guard suites are:

    cd .github/scripts && python3 -m unittest test_bench_dashboard -v
"""

from __future__ import annotations

import importlib.util
import json
import pathlib
import tempfile
import unittest

_SPEC = importlib.util.spec_from_file_location(
    "bench_dashboard", pathlib.Path(__file__).with_name("bench-dashboard.py")
)
dashboard = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(dashboard)


def _order(*revisions):
    """One commit per revision, `None` standing for a record written before the field existed."""
    return [
        {"sha": f"sha{index}", "date": "2026-07-01T00:00:00Z", "reference_revision": revision}
        for index, revision in enumerate(revisions)
    ]


class TestEvalRewriteNote(unittest.TestCase):
    def test_a_revision_change_is_announced(self) -> None:
        (note,) = dashboard.eval_rewrite_note(_order(1, 1, 2, 2))
        self.assertIn("`sha2`", note)
        self.assertIn("re-baseline", note)

    def test_a_steady_series_says_nothing(self) -> None:
        self.assertEqual(dashboard.eval_rewrite_note(_order(2, 2, 2)), [])

    def test_a_gap_in_the_middle_is_not_a_rewrite(self) -> None:
        """The trap: a trend branch cut before the field existed appends a `null` mid-series.

        Comparing across it would announce a second rewrite that never happened — the precise
        misreading this note exists to prevent.
        """
        self.assertEqual(dashboard.eval_rewrite_note(_order(2, None, 2)), [])

    def test_a_real_change_across_a_gap_is_still_announced(self) -> None:
        (note,) = dashboard.eval_rewrite_note(_order(2, None, 3))
        self.assertIn("`sha2`", note)

    def test_the_fields_first_appearance_is_the_rewrite(self) -> None:
        """Commits before it ran a harness that had no revision, which is what the rewrite added."""
        (note,) = dashboard.eval_rewrite_note(_order(None, None, 2))
        self.assertIn("`sha2`", note)

    def test_a_series_that_opens_with_a_revision_announces_nothing(self) -> None:
        """Nothing precedes it, so there is no step — only a first observation."""
        self.assertEqual(dashboard.eval_rewrite_note(_order(2, 2)), [])

    def test_an_empty_series_is_not_an_error(self) -> None:
        self.assertEqual(dashboard.eval_rewrite_note([]), [])


class TestLoadEval(unittest.TestCase):
    def test_the_revision_rides_the_commit_not_the_metric(self) -> None:
        """A rewrite moves every task at once, so the field belongs to the commit."""
        records = [
            {"sha": "aaa", "date": "2026-07-01T00:00:00Z", "task": "tweak",
             "reference_revision": 2, "tokens_total": 17868, "payload_characters": 0},
            {"sha": "aaa", "date": "2026-07-01T00:00:00Z", "task": "repair",
             "reference_revision": 2, "tokens_total": 18023, "payload_characters": 0},
        ]
        with tempfile.TemporaryDirectory() as root:
            path = pathlib.Path(root) / "eval-history.jsonl"
            path.write_text("".join(json.dumps(r) + "\n" for r in records), encoding="utf-8")
            order, series = dashboard.load_eval(str(path))
        self.assertEqual([entry["sha"] for entry in order], ["aaa"])
        self.assertEqual(order[0]["reference_revision"], 2)
        self.assertEqual(series[("payload_characters", "tweak")], {0: 0})

    def test_a_record_predating_the_field_loads_as_none(self) -> None:
        record = {"sha": "bbb", "date": "2026-06-01T00:00:00Z", "task": "tweak",
                  "tokens_total": 17821, "payload_characters": 2097}
        with tempfile.TemporaryDirectory() as root:
            path = pathlib.Path(root) / "eval-history.jsonl"
            path.write_text(json.dumps(record) + "\n", encoding="utf-8")
            order, _ = dashboard.load_eval(str(path))
        self.assertIsNone(order[0]["reference_revision"])


if __name__ == "__main__":
    unittest.main()

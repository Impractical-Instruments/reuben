"""The forcing function for #612: prove roster growth is *visible* but never a verdict.

The gate's whole point after #612 is that a bigger agent surface annotates and rides the trend
without breaking the build — only a broken reference solution or an uncomputable metric fails it. A
test that let a metric regression flip `failed` back to True would silently restore the brake the
map spent a grilling removing, so these assert the demotion head-on, plus the density readout that
replaces the lost signal.

Pure report dicts, no sidecar: `render`/`_classify`/`_density_lines` are string-and-arithmetic.
"""

from __future__ import annotations

import unittest

from reuben_eval import gate


def _tokens(*, total: int, fixed: int, tools: int, schema_bytes: int) -> dict:
    return {
        "total": total,
        "fixed": fixed,
        "tool_count": tools,
        "schemas_bytes": schema_bytes,
        "schema_density": schema_bytes / tools if tools else 0.0,
    }


def _result(*, passed: bool = True, total: int, fixed: int, tools: int, schema_bytes: int,
            failure: str | None = None) -> dict:
    return {
        "passed": passed,
        "tokens": _tokens(total=total, fixed=fixed, tools=tools, schema_bytes=schema_bytes),
        "repair_rounds": 0,
        "payload_characters": 100,
        "failure": failure,
    }


def _report(result: dict) -> dict:
    return {"tier": "gate", "tasks": {"tweak": result}}


class TestVisibilityNotVerdict(unittest.TestCase):
    def test_a_large_grounding_jump_does_not_fail_the_gate(self) -> None:
        """~19 new arms is a >10% fixed-grounding jump — the exact case that must NOT break #603."""
        baseline = _report(_result(total=5000, fixed=4767, tools=8, schema_bytes=8000))
        head = _report(_result(total=6500, fixed=6200, tools=27, schema_bytes=20000))
        summary, failed = gate.render(head, baseline)
        self.assertFalse(failed, "a grounding regression must be visibility, not a verdict")
        self.assertIn("🔺", summary)  # the jump is still shown, just not blocking

    def test_a_broken_reference_solution_still_fails(self) -> None:
        """FAIL survives for exactly one non-decision: the reference solution stopped passing."""
        head = _report(
            _result(passed=False, total=5000, fixed=4767, tools=8, schema_bytes=8000,
                    failure="validate rejected the produced document")
        )
        _, failed = gate.render(head, None)
        self.assertTrue(failed)

    def test_classify_never_returns_a_blocking_tier(self) -> None:
        self.assertEqual(gate._classify(50.0), "jump")
        self.assertEqual(gate._classify(5.0), "creep")
        self.assertEqual(gate._classify(1.0), "ok")
        self.assertEqual(gate._classify(None), "ok")
        self.assertNotIn(gate._classify(999.0), {"FAIL", "fail"})


class TestSchemaDensity(unittest.TestCase):
    def test_growth_by_capability_reads_as_more_tools_flat_density(self) -> None:
        """8 tools @ 1000 B/tool -> 27 tools @ 1000 B/tool: capability growth, density held."""
        baseline = _report(_result(total=5000, fixed=4767, tools=8, schema_bytes=8000))
        head = _report(_result(total=6500, fixed=6200, tools=27, schema_bytes=27000))
        summary, _ = gate.render(head, baseline)
        self.assertIn("Per-tool schema density", summary)
        self.assertIn("more tools", summary)
        self.assertIn("flat density", summary)

    def test_bloat_reads_as_denser_schemas_no_new_tool(self) -> None:
        """Same 8 tools, fatter schemas: the invisible regression the density number surfaces."""
        baseline = _report(_result(total=5000, fixed=4767, tools=8, schema_bytes=8000))
        head = _report(_result(total=5600, fixed=5400, tools=8, schema_bytes=16000))
        summary, _ = gate.render(head, baseline)
        self.assertIn("same roster", summary)
        self.assertIn("denser schemas", summary)

    def test_density_line_is_present_without_a_baseline(self) -> None:
        summary, _ = gate.render(_report(_result(total=5000, fixed=4767, tools=8, schema_bytes=8000)), None)
        self.assertIn("1000 B/tool", summary)


class TestHistoryRecords(unittest.TestCase):
    def test_density_fields_ride_the_trend(self) -> None:
        report = _report(_result(total=5000, fixed=4767, tools=27, schema_bytes=27000))
        (record,) = gate.history_records(report, {"sha": "abc"})
        self.assertEqual(record["tool_count"], 27)
        self.assertEqual(record["schemas_bytes"], 27000)
        self.assertEqual(record["schema_density"], 1000.0)


if __name__ == "__main__":
    unittest.main()

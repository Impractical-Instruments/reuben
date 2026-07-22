"""The deterministic tier: replay every reference solution and report the surface's cost floor.

No inference, no network, no model — so it runs in CI on every PR in seconds and its numbers are
byte-reproducible. What it measures is what the *surface* costs a perfect caller: grounding tokens
handed back by the sidecar, failed-validate rounds, and freehand-JSON characters.

That floor is exactly what a new verb moves, which is why this tier gates and the live tier does
not. see rules: agent-mcp

    python3 -m reuben_eval.gate --json out.json          # measure
    python3 -m reuben_eval.gate --compare base.json      # measure and gate against a baseline
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path
from typing import Any

from . import tasks as task_module
from .runner import run_reference, verify_tokenizer_pins

# Mirrors `.github/scripts/perf-gate.sh` so one idiom covers both trends. see rules: web-product-process
FAIL_PCT = 10.0
WARN_PCT = 3.0

# The gated numbers, in report order. `tokens.fixed` is called out separately from `tokens.total`
# because it is the one that creeps invisibly: every added paragraph of tool description is paid on
# every turn of every task, by every model, forever.
METRICS = (
    ("tokens_total", "grounding tokens", lambda r: r["tokens"]["total"]),
    ("tokens_fixed", "fixed grounding", lambda r: r["tokens"]["fixed"]),
    ("repair_rounds", "repair rounds", lambda r: r["repair_rounds"]),
    ("payload_characters", "document chars", lambda r: r["payload_characters"]),
)


def measure() -> dict[str, Any]:
    pins = verify_tokenizer_pins()
    results = [run_reference(task).as_dict() for task in task_module.TASKS]
    return {
        "tier": "gate",
        "tokenizer": {"encoding": "cl100k_base", "pins": pins},
        "tasks": {result["task"]: result for result in results},
    }


def _delta(current: float, baseline: float) -> float | None:
    """Percent change, or None when the baseline is zero (no meaningful ratio exists)."""
    if baseline == 0:
        return None if current == 0 else float("inf")
    return (current - baseline) / baseline * 100.0


def _classify(delta: float | None) -> str:
    if delta is None or delta != delta:  # None or NaN
        return "ok"
    if delta >= FAIL_PCT:
        return "FAIL"
    if delta >= WARN_PCT:
        return "WARN"
    return "ok"


def render(report: dict[str, Any], baseline: dict[str, Any] | None) -> tuple[str, bool]:
    """Job-summary markdown plus the gate verdict (True = failed)."""
    lines = ["## Agent-surface eval gate — deterministic tier", ""]
    failed = False

    unpassed = [key for key, result in report["tasks"].items() if not result["passed"]]
    if unpassed:
        failed = True
        lines.append(
            "❌ The reference solution no longer passes for: "
            + ", ".join(f"`{key}`" for key in unpassed)
        )
        for key in unpassed:
            lines.append(f"  - `{key}`: {report['tasks'][key]['failure']}")
        lines.append("")
        lines.append(
            "_A reference solution is the ideal call sequence, so this is an engine or fixture "
            "change, not a model failure._"
        )
        lines.append("")

    if baseline is None:
        lines += ["_No baseline — absolute numbers only._", ""]
    else:
        lines += [f"Baseline compared · fail > {FAIL_PCT:g}% · warn > {WARN_PCT:g}%", ""]

    lines += ["| Task | Metric | Value | Baseline | Δ% | |", "|---|---|---:|---:|---:|:---:|"]
    for key, result in report["tasks"].items():
        base_task = (baseline or {}).get("tasks", {}).get(key)
        for _, label, extract in METRICS:
            value = extract(result)
            if base_task is None:
                lines.append(f"| `{key}` | {label} | {value} | — | — | |")
                continue
            base_value = extract(base_task)
            delta = _delta(value, base_value)
            status = _classify(delta)
            icon = {"FAIL": "❌", "WARN": "⚠️", "ok": "✅"}[status]
            if status == "FAIL":
                failed = True
                print(
                    f"::error title=Agent-surface regression::{key} {label} "
                    f"{base_value} -> {value} (+{delta:.1f}%, > {FAIL_PCT:g}%)"
                )
            elif status == "WARN":
                print(
                    f"::warning title=Agent-surface creep::{key} {label} "
                    f"{base_value} -> {value} (+{delta:.1f}%, > {WARN_PCT:g}%)"
                )
            shown = "—" if delta is None else f"{delta:+.1f}"
            lines.append(f"| `{key}` | {label} | {value} | {base_value} | {shown} | {icon} |")

    lines.append("")
    lines.append(
        "**Result: ❌ the agent surface got more expensive.**"
        if failed
        else "**Result: ✅ the agent surface held.**"
    )
    lines.append("")
    return "\n".join(lines), failed


def history_records(report: dict[str, Any], identity: dict[str, str]) -> list[dict[str, Any]]:
    """One JSONL record per task, for `eval-history.jsonl` on the trend branch.

    A distinct shape from `bench-history.jsonl` on purpose: `ir` never holds a token count, so the
    dashboard can never plot the two on one axis by accident.
    """
    return [
        {
            **identity,
            "tier": "gate",
            "task": key,
            "passed": result["passed"],
            "tokens_total": result["tokens"]["total"],
            "tokens_fixed": result["tokens"]["fixed"],
            "repair_rounds": result["repair_rounds"],
            "payload_characters": result["payload_characters"],
        }
        for key, result in report["tasks"].items()
    ]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", type=Path, help="write the full report here")
    parser.add_argument("--compare", type=Path, help="baseline report to gate against")
    parser.add_argument("--history", type=Path, help="write eval-history.jsonl records here")
    parser.add_argument("--summary", type=Path, help="write the markdown summary here")
    parser.add_argument(
        "--no-gate",
        action="store_true",
        help="report but always exit 0 (used for the baseline-side run)",
    )
    args = parser.parse_args(argv)

    report = measure()
    baseline = None
    if args.compare and args.compare.is_file():
        baseline = json.loads(args.compare.read_text(encoding="utf-8"))

    summary, failed = render(report, baseline)
    print(summary)
    if args.summary:
        args.summary.write_text(summary, encoding="utf-8")
    elif os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as handle:
            handle.write(summary + "\n")

    if args.json:
        args.json.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    if args.history:
        identity = {
            "sha": os.environ.get("EVAL_SHA", ""),
            "commit_sha": os.environ.get("EVAL_COMMIT_SHA", ""),
            "date": os.environ.get("EVAL_DATE", ""),
            "run_id": os.environ.get("GITHUB_RUN_ID", ""),
        }
        args.history.write_text(
            "".join(json.dumps(record) + "\n" for record in history_records(report, identity)),
            encoding="utf-8",
        )

    return 1 if (failed and not args.no_gate) else 0


if __name__ == "__main__":
    sys.exit(main())

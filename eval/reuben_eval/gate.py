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
import math
import os
import sys
from pathlib import Path
from typing import Any

from . import tasks as task_module
from .runner import run_reference, verify_tokenizer_pins

# Two VISIBILITY tiers, not verdicts: a grounding metric that regresses annotates and lands on the
# trend, but never fails the build. A gate that FAILs on roster growth encodes "the library must not
# grow" — a non-goal — so only a broken reference solution or an uncomputable metric fails here (see
# `render`). #612. Boundaries are inclusive at `delta >= PCT`, matching perf-gate's awk — hence "≥".
JUMP_PCT = 10.0
CREEP_PCT = 3.0

# The reported numbers, in report order. `tokens.fixed` is called out separately from `tokens.total`
# because it is the one that creeps invisibly: every added paragraph of tool description is paid on
# every turn of every task, by every model, forever. The per-tool schema-density line below keeps
# that creep legible — capability growth holds density flat; bloat pushes it up.
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
    """Visibility tier for a metric delta — never a verdict. `jump` and `creep` both annotate and
    ride the trend; neither fails the build. Only a broken reference solution or an uncomputable
    metric does that (`render`). #612."""
    if delta is None or math.isnan(delta):  # no ratio (zero baseline), or NaN
        return "ok"
    if delta >= JUMP_PCT:
        return "jump"
    if delta >= CREEP_PCT:
        return "creep"
    return "ok"


def _density_lines(report: dict[str, Any], baseline: dict[str, Any] | None) -> list[str]:
    """The per-tool schema-density readout: schema bytes ÷ tool count.

    The one line that separates roster GROWTH (more tools, flat density — a capability decision) from
    schema BLOAT (denser schemas, no new tool — the invisible regression). It is constant across
    tasks (one roster, one sidecar), so it is read from any one of them. #612.
    """
    tasks = report.get("tasks", {})
    if not tasks:
        return []
    head = next(iter(tasks.values()))["tokens"]
    tools, sbytes, density = head["tool_count"], head["schemas_bytes"], head["schema_density"]
    line = f"**Per-tool schema density:** {density:.0f} B/tool — {tools} tools, {sbytes:,} B of schema"

    base_tasks = (baseline or {}).get("tasks", {})
    if base_tasks:
        base = next(iter(base_tasks.values()))["tokens"]
        b_tools = base.get("tool_count")
        b_density = base.get("schema_density")
        if b_tools is not None and b_density is not None:
            # Compare at the precision the reader sees (`:.0f`), so the label can never contradict
            # the two numbers printed beside it — a sub-1-B/tool drift is noise, not "denser".
            d, bd = round(density), round(b_density)
            roster = (
                "more tools" if tools > b_tools else "same roster" if tools == b_tools else "fewer tools"
            )
            shape = (
                "denser schemas" if d > bd else "flat density" if d == bd else "leaner schemas"
            )
            line += f" · baseline {b_density:.0f} B/tool over {b_tools} tools ({roster}, {shape})"
    return ["", line, ""]


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
        lines += [f"Baseline compared · reported for visibility · jump ≥ {JUMP_PCT:g}% · creep ≥ {CREEP_PCT:g}%", ""]

    lines += ["| Task | Metric | Value | Baseline | Δ% | |", "|---|---|---:|---:|---:|:---:|"]
    warned = False
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
            # Both tiers are non-blocking: a bigger surface is a decision to make with eyes open, not
            # a build to break. They annotate and ride the trend; `failed` is untouched here. #612.
            icon = {"jump": "🔺", "creep": "⚠️", "ok": "✅"}[status]
            if status in ("jump", "creep"):
                warned = True
                pct = JUMP_PCT if status == "jump" else CREEP_PCT
                print(
                    f"::warning title=Agent-surface {status}::{key} {label} "
                    f"{base_value} -> {value} (+{delta:.1f}%, ≥ {pct:g}%)"
                )
            shown = "—" if delta is None else f"{delta:+.1f}"
            lines.append(f"| `{key}` | {label} | {value} | {base_value} | {shown} | {icon} |")

    lines += _density_lines(report, baseline)

    lines.append("")
    if failed:
        lines.append("**Result: ❌ a reference solution no longer passes — the surface is broken, not merely bigger.**")
    elif warned:
        lines.append("**Result: ⚠️ the agent surface grew — recorded on the trend, not blocked. See annotations.**")
    else:
        lines.append("**Result: ✅ the agent surface held.**")
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
            "tool_count": result["tokens"]["tool_count"],
            "schemas_bytes": result["tokens"]["schemas_bytes"],
            "schema_density": result["tokens"]["schema_density"],
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

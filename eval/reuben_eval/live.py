"""The live tier: a ladder of hardware bands, run on demand. see rules: agent-mcp

The question this tier answers is **not** "does it pass" but **where the pass line sits**. Rungs are
memory bands — fits in 8 / 16 / 32 GB of unified memory at usable speed — and the model filling each
band is a re-pinnable detail, because "desktop-class" in the destination is a hardware claim, not a
parameter claim. A prototype earns its place by moving the pass line **down a band**.

Never gates CI: it needs a local runner, minutes per task, and its numbers move when a rung is
re-pinned. The deterministic tier is what gates, and it is immune to re-pins by construction.

    OLLAMA_CONTEXT_LENGTH=32768 ollama serve
    python3 -m reuben_eval.live --rung 16gb --json live-16gb.json
"""

from __future__ import annotations

import argparse
import json
import os
import statistics
import sys
import tempfile
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from . import tasks as task_module
from .runner import Session
from .tokenizer import cl100k

# Frozen by #592: capped-out counts as a fail, and 3 repeats at temperature 0 absorb some of the
# sampling noise the ladder is deliberately parked in.
ROUND_CAP = 12
REPEATS = 3
TEMPERATURE = 0.0

# Ollama defaults to a 4k context below 24 GiB of VRAM — which is *every rung on this ladder*. Left
# at the default the low rungs truncate the grounding budget and fail for a reason that has nothing
# to do with the model's ability. It must be set on the SERVER (`OLLAMA_CONTEXT_LENGTH`); the
# OpenAI-compatible endpoint has nowhere to carry it per-request. The harness records what it was
# told and refuses to guess. see docs/research/harness-rungs.md §7
DEFAULT_CONTEXT_LENGTH = 32768


@dataclass(frozen=True)
class Rung:
    """One hardware band, pinned by #597 to IBM Granite 4.1 (Apache-2.0, dense at all three sizes).

    Dense on purpose: the band is decided by *resident* footprint, so a sparse MoE buys a lower band
    in latency and a **higher** one in memory — a net loss for a memory-banded ladder.

    `manifest_sha256` is the pin that matters. Ollama tags are mutable; digests are not.
    """

    band: str
    model: str
    manifest_sha256: str
    weight_bytes: int
    budget_bytes: int


# Weights budget per band = Metal's `recommendedMaxWorkingSetSize` (~66% of RAM at or below 36GB)
# less room for a real KV cache. All three fit with headroom and need no `sudo sysctl`.
RUNGS: dict[str, Rung] = {
    "8gb": Rung(
        band="8gb",
        model="granite4.1:3b-q4_K_M",
        manifest_sha256="6fd349357287c7ffc9e38189a93b48ea175d24fc566b38f09cfc564fb7f303eb",
        weight_bytes=2_099_501_664,
        budget_bytes=4_000_000_000,
    ),
    "16gb": Rung(
        band="16gb",
        model="granite4.1:8b-q4_K_M",
        manifest_sha256="444af1c4b2fedd6b54041aca558e7300b0b3d5c0468c44619126240323ba2852",
        weight_bytes=5_347_914_400,
        budget_bytes=8_000_000_000,
    ),
    "32gb": Rung(
        band="32gb",
        model="granite4.1:30b-q4_K_M",
        manifest_sha256="3f3e5df8a021439fd6f867a0e526bdc303cac79c811201cb6bac193298cb9fcd",
        weight_bytes=17_490_240_736,
        budget_bytes=18_000_000_000,
    ),
}

# The 8GB rung is the weakest pin on the ladder: nothing in the research shows a 3B sustaining a
# 12-round MCP loop against real schemas. If it fails universally on day one, suspect the pin before
# concluding the surface is too expensive. see docs/research/harness-rungs.md §4.2
WEAKEST_RUNG = "8gb"


class EndpointError(RuntimeError):
    """The chat endpoint was unreachable or answered with something unusable."""


def chat(base_url: str, body: dict[str, Any], timeout: float = 600.0) -> dict[str, Any]:
    """POST to an OpenAI-compatible `/chat/completions`. stdlib only — no `openai`, no `requests`."""
    request = urllib.request.Request(
        base_url.rstrip("/") + "/chat/completions",
        data=json.dumps(body).encode("utf-8"),
        headers={
            "Content-Type": "application/json",
            # Ollama ignores it; a hosted OpenAI-compatible endpoint needs it.
            "Authorization": f"Bearer {os.environ.get('REUBEN_EVAL_API_KEY', 'not-needed')}",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as error:
        raise EndpointError(f"{error.code} from the endpoint: {error.read()[:400]!r}") from error
    except urllib.error.URLError as error:
        raise EndpointError(f"could not reach {base_url}: {error.reason}") from error


def run_once(task: task_module.Task, rung: Rung, base_url: str) -> dict[str, Any]:
    """Drive one model through one task. Hitting the round cap is a fail, not a truncation."""
    with tempfile.TemporaryDirectory(prefix=f"reuben-live-{task.key}-") as root:
        with Session(task, Path(root) / "workspace") as session:
            messages: list[dict[str, Any]] = [
                {"role": "system", "content": session.sidecar.instructions},
                {"role": "user", "content": task.prompt},
            ]
            tools = session.tool_definitions()
            capped = False
            sampling: dict[str, Any] = {}

            for _ in range(ROUND_CAP):
                answer = chat(
                    base_url,
                    {
                        "model": rung.model,
                        "messages": messages,
                        "tools": tools,
                        "temperature": TEMPERATURE,
                    },
                )
                choice = (answer.get("choices") or [{}])[0]
                message = choice.get("message") or {}
                # Record what the server said it did, rather than what we asked for: the Ollama Go
                # runner silently discards penalty parameters, so "temperature 0" is not the whole
                # sampling story. see docs/research/harness-rungs.md §7
                sampling = {
                    "requested": {"temperature": TEMPERATURE},
                    "reported_model": answer.get("model"),
                    "finish_reason": choice.get("finish_reason"),
                    "usage": answer.get("usage"),
                }
                messages.append(message)

                calls = message.get("tool_calls") or []
                if not calls:
                    break
                for call in calls:
                    function = call.get("function") or {}
                    name = function.get("name", "")
                    try:
                        arguments = json.loads(function.get("arguments") or "{}")
                    except json.JSONDecodeError as error:
                        # Malformed tool arguments are a real small-model failure mode, and the
                        # model gets to see the error and try again — that is the loop under test.
                        result = f"error: arguments were not valid JSON: {error}"
                    else:
                        result = session.call(name, arguments)
                    messages.append(
                        {"role": "tool", "tool_call_id": call.get("id", ""), "content": result}
                    )
            else:
                capped = True

            outcome = session.judge()
            record = outcome.as_dict()
            if capped:
                record["passed"] = False
                record["failure"] = f"hit the {ROUND_CAP}-round cap without finishing"
            record["capped"] = capped
            record["sampling"] = sampling
            record["trace"] = outcome.trace
            return record


def run_rung(rung: Rung, base_url: str, context_length: int) -> dict[str, Any]:
    """Every task, `REPEATS` times, on one rung.

    Reports pass rate plus the **median over passing runs**: a failed run's numbers describe a
    different journey than a successful one, so averaging them together would be meaningless.
    """
    cl100k.verify_pins()
    tasks: dict[str, Any] = {}
    for task in task_module.TASKS:
        runs = [run_once(task, rung, base_url) for _ in range(REPEATS)]
        passing = [run for run in runs if run["passed"]]
        tasks[task.key] = {
            "shape": task.shape,
            "pass_rate": len(passing) / len(runs),
            "runs": runs,
            "median": {
                metric: statistics.median(extract(run) for run in passing)
                for metric, extract in (
                    ("tokens_total", lambda run: run["tokens"]["total"]),
                    ("rounds", lambda run: run["rounds"]),
                    ("repair_rounds", lambda run: run["repair_rounds"]),
                    ("payload_characters", lambda run: run["payload_characters"]),
                )
            }
            if passing
            else None,
        }
    return {
        "tier": "live",
        "rung": {
            "band": rung.band,
            "model": rung.model,
            "manifest_sha256": rung.manifest_sha256,
            "weight_bytes": rung.weight_bytes,
            "budget_bytes": rung.budget_bytes,
        },
        "discipline": {
            "temperature": TEMPERATURE,
            "round_cap": ROUND_CAP,
            "repeats": REPEATS,
            "context_length": context_length,
        },
        "tasks": tasks,
    }


def render(report: dict[str, Any]) -> str:
    rung = report["rung"]
    lines = [
        f"## Agent-surface eval — live tier, {rung['band']} band",
        "",
        f"Model `{rung['model']}` · manifest `{rung['manifest_sha256'][:12]}…` · "
        f"weights {rung['weight_bytes'] / 1e9:.2f} GB of a {rung['budget_bytes'] / 1e9:.1f} GB budget",
        f"Context {report['discipline']['context_length']} · temperature "
        f"{report['discipline']['temperature']} · {report['discipline']['repeats']} repeats · "
        f"round cap {report['discipline']['round_cap']}",
        "",
        "| Task | Pass rate | Median tokens | Median rounds | Median repairs | Median doc chars |",
        "|---|---:|---:|---:|---:|---:|",
    ]
    for key, result in report["tasks"].items():
        median = result["median"]
        cells = (
            [
                f"{median['tokens_total']:.0f}",
                f"{median['rounds']:.0f}",
                f"{median['repair_rounds']:.0f}",
                f"{median['payload_characters']:.0f}",
            ]
            if median
            else ["—", "—", "—", "—"]
        )
        lines.append(f"| `{key}` | {result['pass_rate']:.0%} | " + " | ".join(cells) + " |")
    lines.append("")
    if rung["band"] == WEAKEST_RUNG and all(r["pass_rate"] == 0 for r in report["tasks"].values()):
        lines.append(
            f"> ⚠️ The `{WEAKEST_RUNG}` rung is the weakest pin on the ladder — nothing in the "
            "research shows a 3B sustaining a 12-round MCP loop against real schemas. A universal "
            "failure here should be read as *suspect the pin* before *the surface is too "
            "expensive*. See `docs/research/harness-rungs.md` §4.2."
        )
        lines.append("")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rung", choices=sorted(RUNGS), required=True)
    parser.add_argument(
        "--base-url",
        default=os.environ.get("REUBEN_EVAL_BASE_URL", "http://localhost:11434/v1"),
        help="an OpenAI-compatible endpoint (default: local Ollama)",
    )
    parser.add_argument(
        "--context-length",
        type=int,
        default=int(os.environ.get("OLLAMA_CONTEXT_LENGTH", DEFAULT_CONTEXT_LENGTH)),
        help="the context the SERVER is running with — recorded, not applied",
    )
    parser.add_argument("--json", type=Path, help="write the full report here")
    args = parser.parse_args(argv)

    if "OLLAMA_CONTEXT_LENGTH" not in os.environ:
        print(
            f"note: OLLAMA_CONTEXT_LENGTH is not set in this environment; recording "
            f"{args.context_length}. Ollama defaults to a 4k context below 24 GiB of VRAM, which "
            "would truncate every rung on this ladder — set it on the server process.",
            file=sys.stderr,
        )

    report = run_rung(RUNGS[args.rung], args.base_url, args.context_length)
    print(render(report))
    if args.json:
        args.json.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())

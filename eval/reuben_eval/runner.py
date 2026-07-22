"""One task run: the surfaces wired together, the three numbers collected. see rules: agent-mcp

Both tiers share this. The gate tier replays a reference solution through it with no inference; the
live tier lets a model choose the calls. Identical accounting either way, which is what makes the
floor and the live number comparable.

The three numbers (#592's yardstick):

- **(a) tokens/turn** — everything the sidecar hands back, tokenized with the pinned vendored
  cl100k_base: server `instructions`, tool schemas, resources read, every tool result. Counted off
  the wire, never estimated.
- **(b) validate-repair rounds** — how many `validate` calls came back `ok:false`. The repair task's
  floor is 1: its first validate *is* the diagnosis.
- **(c) freehand-JSON characters** — document-payload characters the model emitted, echoes included.
"""

from __future__ import annotations

import tempfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from . import tasks as task_module
from .mcp import Sidecar
from .tokenizer import cl100k
from .workspace import GUIDE_URIS, HOST_TOOLS, Workspace


@dataclass
class Trace:
    """What happened, in order — the record a failing run is debugged from."""

    calls: list[dict[str, Any]] = field(default_factory=list)

    def record(self, surface: str, name: str, arguments: dict[str, Any], result: str) -> None:
        self.calls.append(
            {
                "surface": surface,
                "name": name,
                # Document payloads are elided: a trace carrying four copies of an instrument is
                # unreadable, and the payload is already priced in metric (c).
                "arguments": {
                    key: (f"<{len(value)} chars>" if key == "content" else value)
                    for key, value in arguments.items()
                },
                "result_preview": result[:240],
            }
        )


@dataclass
class Outcome:
    task: str
    shape: str
    passed: bool
    rounds: int
    tokens: dict[str, Any]
    repair_rounds: int
    payload_characters: int
    payload_per_tool: dict[str, int]
    failure: str | None = None
    trace: list[dict[str, Any]] = field(default_factory=list)

    def as_dict(self) -> dict[str, Any]:
        return {
            "task": self.task,
            "shape": self.shape,
            "passed": self.passed,
            "rounds": self.rounds,
            "tokens": self.tokens,
            "repair_rounds": self.repair_rounds,
            "payload_characters": self.payload_characters,
            "payload_per_tool": self.payload_per_tool,
            "failure": self.failure,
        }


class Session:
    """A sidecar plus a workspace, dispatching one tool namespace across both."""

    def __init__(self, task: task_module.Task, root: Path) -> None:
        self.task = task
        self.workspace = Workspace(root, task.seed)
        self.sidecar = Sidecar(self.workspace.root)
        self.trace = Trace()
        self.repair_rounds = 0
        self.rounds = 0

    def __enter__(self) -> Session:
        self.sidecar.__enter__()
        return self

    def __exit__(self, *exc: object) -> None:
        self.sidecar.__exit__(*exc)

    def tool_definitions(self) -> list[dict[str, Any]]:
        """The model's whole namespace: the sidecar's roster plus the host's file tools."""
        return self.sidecar.openai_tools() + HOST_TOOLS

    def call(self, name: str, arguments: dict[str, Any]) -> str:
        """Route one call to whichever surface owns it, charging both ledgers."""
        self.rounds += 1
        if name == "read_guide":
            uri = str(arguments.get("uri", ""))
            if uri not in GUIDE_URIS:
                result = f"error: no such guide `{uri}`; try one of {', '.join(GUIDE_URIS)}"
            else:
                result = self.sidecar.read_resource(uri)
            self.trace.record("resource", name, arguments, result)
            return result
        if name in {"read_file", "write_file"}:
            result = self.workspace.call(name, arguments)
        elif name in self.sidecar.tools:
            self.workspace.payloads.charge(name, arguments)
            answer = self.sidecar.call_tool(name, arguments)
            result = answer.rendered()
            # Metric (b). Read off the structured report, never the prose: "invalid: 1 error(s)" is
            # a human string a wording change could silently stop matching.
            if name == "validate" and (answer.structured or {}).get("ok") is False:
                self.repair_rounds += 1
        else:
            # An invented tool name still costs a round — that is a real failure mode of small
            # models on a wide surface, and hiding it would flatter the measurement.
            result = f"error: no such tool `{name}`"
        self.trace.record("host" if name in {"read_file", "write_file"} else "mcp", name, arguments, result)
        return result

    def read_resource(self, uri: str) -> str:
        self.rounds += 1
        text = self.sidecar.read_resource(uri)
        self.trace.record("resource", uri, {}, text)
        return text

    def judge(self) -> Outcome:
        """Score the run: `validate` clean on the produced document, then the structural assertion.

        `validate` is called here by the harness itself, not trusted from the transcript — a model
        that validated an earlier draft and then broke the file must not score a pass.
        """
        # Snapshot the ledger BEFORE scoring: the harness's own adjudicating `validate` is not a
        # cost the model paid, and folding it into metric (a) would tax every task by a constant.
        tokens = self.sidecar.ledger.as_dict()
        failure: str | None = None
        try:
            document = self.workspace.read_document(self.task.document)
            verdict = self.sidecar.call_tool("validate", {"path": self.task.document})
            if (verdict.structured or {}).get("ok") is not True:
                failure = f"validate rejected the produced document: {verdict.rendered()[:300]}"
            else:
                self.task.assertion(document)
        except AssertionError as error:
            failure = str(error)
        except Exception as error:  # a crash is a failed run, not a crashed harness
            failure = f"{type(error).__name__}: {error}"

        return Outcome(
            task=self.task.key,
            shape=self.task.shape,
            passed=failure is None,
            rounds=self.rounds,
            tokens=tokens,
            repair_rounds=self.repair_rounds,
            payload_characters=self.workspace.payloads.characters,
            payload_per_tool=self.workspace.payloads.as_dict()["per_tool"],
            failure=failure,
            trace=self.trace.calls,
        )


def run_reference(task: task_module.Task) -> Outcome:
    """Replay a task's reference solution with **no inference** — the surface's cost floor.

    This is what makes a prototype's claim checkable before buying a token: `nudge("warmer")`
    collapses the floor for the nudge task whether or not any model is smart enough to use it.
    """
    with tempfile.TemporaryDirectory(prefix=f"reuben-eval-{task.key}-") as root:
        with Session(task, Path(root) / "workspace") as session:
            for step in task.reference:
                if step.surface == "resource":
                    session.read_resource(str(step.arguments["uri"]))
                else:
                    session.call(step.name, dict(step.arguments))
            return session.judge()


def verify_tokenizer_pins() -> dict[str, str]:
    """Fail before any measurement if a vendored tokenizer artifact drifted."""
    return cl100k.verify_pins()

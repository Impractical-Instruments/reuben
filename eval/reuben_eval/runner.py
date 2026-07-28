"""One task run: the surfaces wired together, the three numbers collected. see rules: agent-mcp

Both tiers share this. The gate tier replays a reference solution through it with no inference; the
live tier lets a model choose the calls. Identical accounting either way, which is what makes the
floor and the live number comparable.

The three numbers the ladder is measured in:

- **(a) tokens/turn** — everything the sidecar hands back, tokenized with the pinned vendored
  cl100k_base: server `instructions`, tool schemas, resources read, every tool result. Counted off
  the wire, never estimated.
- **(b) validate-repair rounds** — how many `validate_instrument` calls came back `ok:false`. The repair task's
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
from .workspace import (
    FILE_ACCESS,
    GUIDE_URIS,
    HOST_TOOLS,
    Workspace,
    file_access_failure,
    looks_like_file_access,
)


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
    # How the run failed, when the harness can classify it: `FILE_ACCESS` for a reach at the retired
    # file tools, `None` for everything else. Carried apart from `failure` so a report can group by
    # it without matching on prose.
    failure_mode: str | None = None
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
            "failure_mode": self.failure_mode,
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
        self.file_access_reaches: list[str] = []

    def __enter__(self) -> Session:
        self.sidecar.__enter__()
        return self

    def __exit__(self, *exc: object) -> None:
        self.sidecar.__exit__(*exc)

    def tool_definitions(self) -> list[dict[str, Any]]:
        """The model's whole namespace: the sidecar's roster plus the host's `read_guide`."""
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
        # Charged before dispatch, so a document emitted at a tool that refuses it is priced rather
        # than refunded by the error — the reach costs what taking it would have cost.
        self.workspace.payloads.charge(name, arguments)
        surface = "mcp"
        if name in self.sidecar.tools:
            # The roster gets first claim on every name, so nothing below can shadow a real verb.
            answer = self.sidecar.call_tool(name, arguments)
            result = answer.rendered()
            # Metric (b). Read off the structured report, never the prose: "invalid: 1 error(s)" is
            # a human string a wording change could silently stop matching.
            if name == "validate_instrument" and (answer.structured or {}).get("ok") is False:
                self.repair_rounds += 1
        elif looks_like_file_access(name):
            # Classified apart from a malformed call: this one is a model asking for the retired
            # path, which is the behaviour the tier exists to detect. `judge` names it.
            self.file_access_reaches.append(name)
            surface = "host"
            result = f"error: no such tool `{name}`"
        else:
            # An invented tool name still costs a round — that is a real failure mode of small
            # models on a wide surface, and hiding it would flatter the measurement.
            result = f"error: no such tool `{name}`"
        self.trace.record(surface, name, arguments, result)
        return result

    def read_resource(self, uri: str) -> str:
        self.rounds += 1
        text = self.sidecar.read_resource(uri)
        self.trace.record("resource", uri, {}, text)
        return text

    def judge(self) -> Outcome:
        """Score the run: no reach for a file, `validate_instrument` clean, then the assertion.

        `validate_instrument` is called here by the harness itself, not trusted from the transcript — a model
        that validated an earlier draft and then broke the file must not score a pass.
        """
        # Snapshot the ledger BEFORE scoring: the harness's own adjudicating `validate_instrument` is not a
        # cost the model paid, and folding it into metric (a) would tax every task by a constant.
        tokens = self.sidecar.ledger.as_dict()
        failure: str | None = None
        try:
            document = self.workspace.read_document(self.task.document)
            verdict = self.sidecar.call_tool("validate_instrument", {"source": self.task.document})
            if (verdict.structured or {}).get("ok") is not True:
                failure = f"validate_instrument rejected the produced document: {verdict.rendered()[:300]}"
            else:
                self.task.assertion(document)
        except AssertionError as error:
            failure = str(error)
        except Exception as error:  # a crash is a failed run, not a crashed harness
            failure = f"{type(error).__name__}: {error}"

        # Outranks whatever the document ended up looking like: a run that reached for a file tool
        # has failed the thing this tier guarantees even when what it left behind validates, and
        # reporting the downstream symptom instead would send the reader to the wrong place.
        reach = file_access_failure(self.file_access_reaches)

        return Outcome(
            task=self.task.key,
            shape=self.task.shape,
            passed=failure is None and reach is None,
            rounds=self.rounds,
            tokens=tokens,
            repair_rounds=self.repair_rounds,
            payload_characters=self.workspace.payloads.characters,
            payload_per_tool=self.workspace.payloads.as_dict()["per_tool"],
            failure=reach or failure,
            failure_mode=FILE_ACCESS if reach else None,
            trace=self.trace.calls,
        )


def run_reference(task: task_module.Task) -> Outcome:
    """Replay a task's reference solution with **no inference** — the surface's cost floor.

    This is what makes a prototype's claim checkable before buying a token: one intent word in
    collapses the floor for the intent-word tasks whether or not any model is smart enough to use
    it.
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

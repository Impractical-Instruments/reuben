"""The task workspace and the host tools the model can reach. see rules: agent-mcp

**Why there is no file tool here.** There was, and it was a measurement rather than a crutch: a real
client still has `Read`/`Write`, so leaving them on the namespace was what let the harness watch a
model reach for them anyway. That observation has been traded for a stronger one. The roster reads a
document (`describe_instrument`) and writes one (the document verbs) without the model ever seeing
its bytes, so a conforming client has no reason to touch instrument JSON at all — and the harness now
detects the **reach** rather than the completed operation, which separates what a model *wants* from
what the surface happens to offer it.

That detector is live, not belt-and-braces on something already impossible. Removing the tools from
this roster does not remove them from the world: `source` is still a filesystem path on the native
and sidecar doors, and a real host brings its own read and write that reuben cannot take away. So the
old path stays walkable outside the harness, and a model that still prefers it says so here.

`read_guide` is not a file tool and is not part of that trade: it reads grounding prose that is meant
for the model's context, not a reuben-owned document.

**Seeding is not a tool.** A task's fixture — the `repair` task's broken document — is written by
`Workspace` at construction, on the harness's side of the wire. It is structurally out of the model's
reach rather than conventionally so: nothing in `HOST_TOOLS` can read or write a file, and there is
no host dispatch for one to route to.

**Metric (c) is collected here.** Document-payload characters are counted on every argument that
carries an instrument document or a fragment of one, **including echoes**. A model that copies a
document out of a tool result and back into the next call pays full price, because killing that
re-emit is the single largest win on the table.
"""

from __future__ import annotations

import json
import re
import shutil
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

# Tool arguments that carry an instrument document or a fragment of one. Anything not named here —
# an intent word, a node address, a float, a `path` — costs metric (c) nothing, which is the whole
# point of the metric: it prices freehand JSON, not communication.
#
# `write_file` charges ALL of `content`, not just content routed to the answer document. Deliberate:
# every task writes exactly one file — the instrument document — so in practice there is nothing else
# to write, and any content the model does emit is freehand structured text it had to produce. The
# error only ever runs one way (a stray scratch write makes the surface look MORE expensive, never
# less), so it cannot flatter a prototype's claim — the direction the metric must never be fooled in.
#
# `write_file` is no longer on the roster, and the sidecar's inline `document` arms —
# `validate(document=…)` and `describe_instrument(document=…)` — are gone, so nothing the model can
# successfully call carries a document. It stays named here because a model can still *invent* the
# call: an emission charged at a tool that refuses it is priced rather than refunded by the error, so
# the reach costs what it would have cost. A run that reaches zero has stopped emitting instrument
# JSON altogether, which is now the only outcome a passing run can have.
DOCUMENT_ARGUMENTS: dict[str, tuple[str, ...]] = {
    "write_file": ("content",),
}

# The two names the harness used to offer. They must appear in no call — not on the roster, not in a
# reference solution, not in a live model's trace.
FILE_TOOLS = ("read_file", "write_file")

# The `FILE_ACCESS` classification is a match on both halves: an action word and a filesystem noun.
# Shape-matched rather than enumerated, because the point is to catch the reach *after* the names are
# gone, and what a model emits then is whatever its priors call the same move — `readFile`,
# `fs_write`, `open_file`, `list_files`. Consulted only once the real roster has failed to claim the
# name, so it can never shadow a verb.
_FILE_ACTIONS = frozenset(
    {"read", "write", "open", "create", "edit", "save", "append", "delete", "list"}
)
_FILE_OBJECTS = frozenset({"file", "files", "path", "paths", "dir", "directory", "fs", "disk"})

# The report's name for this outcome, kept out of the prose so a reader and a grep agree on it.
FILE_ACCESS = "file-access"


def looks_like_file_access(name: str) -> bool:
    """Is this unrecognized tool name a reach for the filesystem?

    Never asked about a name the roster claims, and never about `read_guide`: a guide is grounding
    prose meant for the model's context, not a reuben-owned document.
    """
    words = set(re.split(r"[^a-z0-9]+", re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "_", name).lower()))
    return bool(words & _FILE_ACTIONS) and bool(words & _FILE_OBJECTS)


def file_access_failure(reached: list[str]) -> str | None:
    """The named failure: an agent tried to read or write a file. `None` when none did.

    It is the *attempt* that fails a run, not a completed operation — nothing here can complete one.
    Removing the tools from reuben's roster does not remove them from the world, so this stays a live
    detector of whether a model still wants the old path once the surface stops offering it.
    """
    if not reached:
        return None
    names = ", ".join(f"`{name}`" for name in sorted(set(reached)))
    return (
        f"{FILE_ACCESS}: an agent tried to read or write a file ({names}) — a document is read with "
        "`describe_instrument` and written with the document verbs; its bytes never reach the agent"
    )


# MCP resources are not tools, so a client has to surface them to the model somehow — Claude Code
# offers them as an explicit fetch. Modelled the same way here, and routed straight to
# `resources/read` so the bytes are charged to the resource bucket in both tiers.
GUIDE_URIS = (
    "reuben://guide/authoring",
    "reuben://guide/vocabulary",
    "reuben://guide/library-index",
)

HOST_TOOLS: list[dict[str, Any]] = [
    {
        "type": "function",
        "function": {
            "name": "read_guide",
            "description": (
                "Read one of the reuben grounding documents: `reuben://guide/authoring` (type "
                "system, wiring rules, instrument format), `reuben://guide/vocabulary` (the "
                "word→move table for intent language like \"warmer\" or \"busier\"), or "
                "`reuben://guide/library-index` (instruments available to reuse)."
            ),
            "parameters": {
                "type": "object",
                "properties": {"uri": {"type": "string", "enum": list(GUIDE_URIS)}},
                "required": ["uri"],
            },
        },
    },
]


class WorkspaceError(RuntimeError):
    """A workspace path that escaped the root."""


@dataclass
class PayloadLedger:
    """Metric (c): document-payload characters the model emitted, echoes included."""

    characters: int = 0
    per_tool: dict[str, int] = field(default_factory=dict)

    def charge(self, tool: str, arguments: dict[str, Any]) -> None:
        fields = DOCUMENT_ARGUMENTS.get(tool)
        if not fields:
            return
        for name in fields:
            value = arguments.get(name)
            if value is None:
                continue
            # A document may arrive as a JSON string (`write_file`) or as a parsed object. Both are
            # the same emission; normalise so neither is cheaper by accident of encoding.
            text = value if isinstance(value, str) else json.dumps(value, separators=(",", ":"))
            self.characters += len(text)
            self.per_tool[tool] = self.per_tool.get(tool, 0) + len(text)

    def as_dict(self) -> dict[str, Any]:
        return {"characters": self.characters, "per_tool": dict(sorted(self.per_tool.items()))}


class Workspace:
    """A scratch directory seeded with a task's files. Nothing the model calls reaches into it."""

    def __init__(self, root: Path, seed: dict[str, str]) -> None:
        self.root = root
        self.root.mkdir(parents=True, exist_ok=True)
        for relative, content in seed.items():
            target = self._resolve(relative)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(content, encoding="utf-8")
        self.payloads = PayloadLedger()

    def _resolve(self, relative: str) -> Path:
        """Join under the root, refusing anything that escapes it."""
        candidate = (self.root / relative).resolve()
        if candidate != self.root.resolve() and self.root.resolve() not in candidate.parents:
            raise WorkspaceError(f"path escapes the workspace: {relative}")
        return candidate

    def read_document(self, relative: str) -> dict[str, Any]:
        """Parse the task's answer document. A missing or unparseable file is a task failure."""
        path = self._resolve(relative)
        if not path.is_file():
            raise AssertionError(f"the task produced no `{relative}`")
        try:
            return json.loads(path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as error:
            raise AssertionError(f"`{relative}` is not valid JSON: {error}") from error

    def destroy(self) -> None:
        shutil.rmtree(self.root, ignore_errors=True)

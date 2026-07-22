"""The task workspace and the two host file tools. see rules: agent-mcp

**Why the harness ships file tools at all.** The sidecar's `swap` is path-only and there is no
document-read tool on the roster, so a real authoring client necessarily brings its own
filesystem access — that is precisely the file-sightedness `#no-resource-bytes` mandates and #583
proposes to reverse. Modelling that client is not inventing a fourth door: the reuben surface being
measured is still exactly the sidecar's roster. `read_file`/`write_file` stand in for the host's
`Read`/`Write`, and they are deliberately dumb — whole-document in, whole-document out — because
`#whole-document-edit` is the constraint under measurement, not one the harness may quietly relax.

**Metric (c) is collected here.** Document-payload characters are counted on every argument whose
schema type is an instrument document or a fragment of one — `write_file(content=…)` and
`validate(document=…)` alike — **including echoes**. A model that copies a document out of a tool
result and back into the next call pays full price, because killing that re-emit is the single
largest win #576 and #583 claim.
"""

from __future__ import annotations

import json
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
DOCUMENT_ARGUMENTS: dict[str, tuple[str, ...]] = {
    "write_file": ("content",),
    "validate": ("document",),
    "describe_instrument": ("document",),
}

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
    {
        "type": "function",
        "function": {
            "name": "read_file",
            "description": (
                "Read a UTF-8 text file from the working directory. Use it to see an instrument "
                "document before editing it."
            ),
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path relative to the working directory.",
                    }
                },
                "required": ["path"],
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": "write_file",
            "description": (
                "Write a UTF-8 text file in the working directory, replacing it entirely. This is "
                "how an edited instrument document becomes durable."
            ),
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path relative to the working directory.",
                    },
                    "content": {"type": "string", "description": "The complete new file contents."},
                },
                "required": ["path", "content"],
            },
        },
    },
]


class WorkspaceError(RuntimeError):
    """A host file call that escaped the workspace or named a missing file."""


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
            # A document may arrive as a JSON string (`write_file`) or as a parsed object
            # (`validate(document=…)`). Both are the same emission; normalise so the two doors
            # are priced identically and neither is cheaper by accident of encoding.
            text = value if isinstance(value, str) else json.dumps(value, separators=(",", ":"))
            self.characters += len(text)
            self.per_tool[tool] = self.per_tool.get(tool, 0) + len(text)

    def as_dict(self) -> dict[str, Any]:
        return {"characters": self.characters, "per_tool": dict(sorted(self.per_tool.items()))}


class Workspace:
    """A scratch directory seeded with a task's files, plus the host tools that reach into it."""

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

    def call(self, name: str, arguments: dict[str, Any]) -> str:
        """Run a host tool. Errors come back as text, the way a tool result would."""
        self.payloads.charge(name, arguments)
        try:
            if name == "read_file":
                return self._resolve(str(arguments["path"])).read_text(encoding="utf-8")
            if name == "write_file":
                target = self._resolve(str(arguments["path"]))
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text(str(arguments["content"]), encoding="utf-8")
                return f"wrote {arguments['path']}"
        except FileNotFoundError:
            return f"error: no such file: {arguments.get('path')}"
        except WorkspaceError as error:
            return f"error: {error}"
        except KeyError as error:
            return f"error: missing argument {error}"
        raise WorkspaceError(f"unknown host tool: {name}")

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

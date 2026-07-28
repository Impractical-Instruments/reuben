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
carries an instrument document, **including echoes**. A model that copies a document out of a tool
result and back into the next call pays full price, because killing that re-emit is the single
largest win on the table.

A *verb argument* is not one of those, even when it is structured: `add_instrument_node(inputs=…)`
and `send_live_controls(messages=…)` cost nothing. The metric prices **freehand JSON** — the document
the model had to compose and hold — not communication. A node's inputs map is small, named by a
schema, and is the thing the verbs exist to make cheap; charging it would price the cure as the
disease. What counts is a whole document arriving in an argument, at a roster arm that takes one by
value (there are none left) or at a call the model invented.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

# Roster tools whose arguments carry an instrument document. Empty, and that emptiness is the
# statement: `validate(document=…)` and `describe_instrument(document=…)` are gone, so no call the
# model can make successfully carries a document at all. If an arm by value ever returns, name it
# here — that is what keeps metric (c) from silently reading zero through it.
DOCUMENT_ARGUMENTS: dict[str, tuple[str, ...]] = {}

# Argument names that carry a document when the call itself is invented. A refused call is bound by
# no schema, so the *name* it hangs the bytes on is as free as the encoding — pricing only
# `write_file(content=…)` would let `writeFile(content=…)` and `write_file(text=…)` emit a whole
# document for free, and a false zero is the one direction this metric must never be fooled in. All
# of the argument is charged, not just the part routed to the answer document: a stray scratch write
# makes the surface look MORE expensive, never less, so it cannot flatter a prototype's claim.
DOCUMENT_SHAPED_ARGUMENTS = frozenset(
    {"content", "contents", "text", "document", "file_text", "body", "new_str", "new_string", "data"}
)

# The report's name for this outcome, kept out of the prose so a reader and a grep agree on it.
FILE_ACCESS = "file-access"

# A filesystem noun anywhere in the name, OR the whole name being a known host file tool. A UNION,
# not an intersection: an AND of action-and-noun is a stricter gate than the two literal names it
# replaced, and it misses the very tools the rationale above names — `Read`, `Write`, `Edit` are bare
# verbs with no noun to pair with, and `bash`/`str_replace_editor` have neither half. Those are what
# a real host actually offers, so missing them would let a model fall back to the retired path and
# still be reported as having stopped wanting it.
_FILE_NOUNS = frozenset(
    {"file", "files", "filesystem", "path", "paths", "dir", "dirs", "directory", "directories",
     "fs", "disk", "folder", "document", "documents"}
)
# Whole-name matches, for the tools whose names carry neither half — a bare verb (`Read`), a shell
# (`bash`), or a proper noun (`str_replace_editor`). Compared against the name flattened of
# separators too, so `strReplaceEditor` lands with `str_replace_editor`. Deliberately excludes
# ambiguous bare verbs like `create` and `run`, which a model is as likely to aim at a document verb
# as at a file.
_HOST_FILE_TOOLS = frozenset(
    {"read", "write", "edit", "view", "cat", "open", "save", "append", "ls", "glob",
     "bash", "sh", "shell", "exec", "execute", "runcommand", "terminal"}
)

# Fragments that name a file editor whatever it is wrapped in — `str_replace_editor`,
# `str_replace_based_edit_tool`, `text_editor`. A whole-name set cannot keep up with those; the
# family is stable even as the spelling is not.
_FILE_MARKERS = ("strreplace", "editor")


def _words(name: str) -> set[str]:
    """The name's parts, camelCase and any separator alike: `str_replace_editor`, `readFile`."""
    return set(re.split(r"[^a-z0-9]+", re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "_", name).lower()))


def looks_like_file_access(name: str) -> bool:
    """Does this tool name reach for the filesystem?

    Only ever asked about a name the roster has already refused, which is what makes a generous
    matcher safe: it cannot shadow a verb, so the cost of a false positive is a failed run on a call
    that was going to fail anyway, while the cost of a false negative is a green report claiming a
    model stopped wanting the retired path when it did not.
    """
    flat = re.sub(r"[^a-z0-9]+", "", name.lower())
    if flat in _HOST_FILE_TOOLS or any(marker in flat for marker in _FILE_MARKERS):
        return True
    return bool(_words(name) & _FILE_NOUNS)


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

    def charge(self, tool: str, arguments: dict[str, Any], *, on_roster: bool) -> None:
        """Price one call. A roster tool is charged by name; a refused one, by argument shape.

        The split is the whole point. A roster call is bound by a schema, so what carries a document
        is a fact the harness can name in advance. A refused call is bound by nothing, so neither the
        tool name nor the argument name can be assumed — only the shape can.
        """
        fields = DOCUMENT_ARGUMENTS.get(tool, ()) if on_roster else DOCUMENT_SHAPED_ARGUMENTS
        for name in fields:
            value = arguments.get(name)
            if value is None:
                continue
            # A document may arrive as a JSON string or as a parsed object. Both are the same
            # emission; normalise so neither is cheaper by accident of encoding.
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

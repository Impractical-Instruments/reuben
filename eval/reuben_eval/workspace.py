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
disease.

Which is why the two halves are priced differently. A roster call is charged **by name**, because a
schema says in advance what carries a document, and that is what keeps a verb's own arguments free.
A refused call is charged **by value** — read the argument, not its label. Nothing else survives
contact: `apply_patch(patch=…)`, `write_file(text=…)` and `Write(content=…)` are one emission wearing
three spellings, and any list of argument names is one agent product behind the next one shipped.
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

# The size at which an argument stops being communication and becomes a payload. Every argument the
# roster's own verbs take is far below it — an address, a port name, a float, a source path, a node's
# inputs map — and every instrument document is far above. All of the argument is charged, not just
# the part routed to the answer document: a stray scratch write makes the surface look MORE
# expensive, never less, so the error cannot flatter a prototype's claim.
DOCUMENT_CHARACTER_FLOOR = 200

# The keys that identify an instrument document too small to trip the floor.
_DOCUMENT_KEYS = frozenset({"nodes", "format_version", "instrument", "interface"})

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
# Whole-name matches, for the tools whose names carry no noun — a bare verb (`Read`, `Write`,
# `Edit`) or a shell spelled exactly (`run_command`). Compared against the name flattened of
# separators, so `strReplaceEditor` lands with `str_replace_editor` and `run_command` lands without
# `send_command` coming with it. Deliberately excludes ambiguous bare verbs like `create` and `run`,
# which a model is as likely to aim at a document verb as at a file.
_HOST_FILE_TOOLS = frozenset(
    {"read", "write", "edit", "view", "cat", "open", "save", "append", "ls", "glob", "runcommand"}
)

# A shell is a file tool wearing a different hat — `cat instrument.json` reads the document as surely
# as `read_file` does. Matched per WORD rather than whole-name, because the category words are not
# what agent products actually ship: `execute_command` (Cline), `run_shell_command` (Gemini CLI),
# `run_terminal_cmd` (Cursor), `execute_bash` (OpenHands), `local_shell` (OpenAI) all carry a shell
# token inside a longer name. A whole-name set catches the categories and misses every real one —
# structurally the same defect as missing `Read` and `Write`.
#
# Bare `command` is deliberately absent: it is redundant (every product above lands on `execute`,
# `shell`, `terminal`, `cmd` or `bash`) and it collides with `send_live_controls`' likeliest mangles,
# `send_command` and `run_control_command`.
_SHELL_WORDS = frozenset(
    {"bash", "sh", "zsh", "shell", "exec", "execute", "cmd", "terminal", "subprocess",
     "interpreter", "python"}
)

# Editing a document's text is reaching for its bytes, whether or not the name says "file":
# `apply_patch` is OpenAI Codex's editor and mentions neither. Matched per word, like the shells.
_EDIT_WORDS = frozenset({"patch", "diff", "rewrite", "overwrite", "replace"})

# reuben's own naming convention, and the one word that tells a hallucinated document verb apart from
# a host tool. Every verb on the roster is `verb_instrument_object`; no file tool anywhere carries it.
# Without this guard the shell and edit words turn reuben's own vocabulary against it: `patch` is a
# live argument name on `add_instrument_node`, the authoring skill is called *patcher*, and
# `replace_instrument_node` is a natural sibling of the shipped `rename_instrument_node`. Each is a
# malformed call at a document verb, and counting it as a reach corrupts the one number this tier
# exists to produce. The live-roster test cannot catch that — it fires only if reuben *ships* such a
# verb, never if a model hallucinates one, which is exactly the live-tier case.
_OWN_VOCABULARY = "instrument"

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
    words = _words(name)
    # A filesystem noun still counts inside reuben's own vocabulary — `write_instrument_file` names
    # a file whatever else it says. The verb words do not: there, `instrument` is the tell.
    vocabulary = _FILE_NOUNS if _OWN_VOCABULARY in words else _FILE_NOUNS | _SHELL_WORDS | _EDIT_WORDS
    return bool(words & vocabulary)


def file_access_failure(reached: list[str]) -> str | None:
    """The named failure: an agent reached outside the roster for the document. `None` if none did.

    It is the *attempt* that fails a run, not a completed operation — nothing here can complete one.
    The message says so: it names the call the model reached for and stops there. Asserting a
    filesystem operation would over-claim, because a shell or an interpreter is classified on the
    same evidence and no read or write was ever observed.

    Removing the tools from reuben's roster does not remove them from the world, so this stays a live
    detector of whether a model still wants the old path once the surface stops offering it.
    """
    if not reached:
        return None
    names = ", ".join(f"`{name}`" for name in sorted(set(reached)))
    return (
        f"{FILE_ACCESS}: an agent reached outside the roster for the document ({names}) — it is read "
        "with `describe_instrument` and written with the document verbs; its bytes never reach the "
        "agent"
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


def _is_document_payload(value: Any) -> bool:
    """Is this argument value an emitted document, judged on the value alone?

    Two ways to qualify, and neither looks at what the argument is called. Size is the blunt one and
    does most of the work: past `DOCUMENT_CHARACTER_FLOOR` an argument has stopped being
    communication whatever it is, and charging it errs in the safe direction. The document keys catch
    the rest — a near-empty instrument is only a few dozen characters and would slip under the floor.
    """
    if isinstance(value, str):
        if len(value) > DOCUMENT_CHARACTER_FLOOR:
            return True
        try:
            parsed = json.loads(value)
        except (json.JSONDecodeError, ValueError):
            return False
    elif isinstance(value, (dict, list)):
        if len(json.dumps(value, separators=(",", ":"))) > DOCUMENT_CHARACTER_FLOOR:
            return True
        parsed = value
    else:  # a number, a bool, None — never a document
        return False
    return isinstance(parsed, dict) and bool(_DOCUMENT_KEYS & set(parsed))


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
        is a fact the harness can name in advance — and the name is what keeps a verb's own
        structured arguments free. A refused call is bound by nothing, so neither the tool name nor
        the argument name can be assumed: `apply_patch(patch=…)`, `write_file(text=…)` and
        `Write(content=…)` are the same emission wearing three spellings, and any list of names is
        one agent product behind. Only the value itself is a fact, so the value is what is read.
        """
        if on_roster:
            emissions = [
                value
                for name in DOCUMENT_ARGUMENTS.get(tool, ())
                if (value := arguments.get(name)) is not None
            ]
        else:
            emissions = [value for value in arguments.values() if _is_document_payload(value)]
        for value in emissions:
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

"""A minimal MCP stdio client that drives the real `reuben-mcp` sidecar. see rules: agent-mcp

Both tiers go through this, so the surface being measured is **the actual door** — real tool
schemas, the real server `instructions`, real `Report`/`Diag` shapes. Nothing can drift from what a
user's client sees, and the grounding budget is counted for free because it arrives over the wire.
No fourth door is invented in order to be measured.

Everything the server hands back is accumulated in a `Ledger` as it arrives. That is the whole
measurement apparatus for metric (a): the harness never estimates a payload it did not receive.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import threading
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from .tokenizer import cl100k

PROTOCOL_VERSION = "2025-11-25"
REPO = Path(__file__).resolve().parent.parent.parent
# Long enough that a debug-build cold start or a big `describe_operators` never reads as a hang,
# short enough that a wedged sidecar fails the run instead of the job's 6-hour ceiling.
TIMEOUT_SECONDS = 120.0


class SidecarError(RuntimeError):
    """The sidecar died, timed out, or answered a request with a JSON-RPC error."""


@dataclass(frozen=True)
class ToolResult:
    """One `tools/call` answer, kept in both the shapes the sidecar sends.

    `text` is what a model reads; `structured` is what the harness scores against — a verdict must
    never be parsed out of prose that a wording change could break.
    """

    text: str
    structured: dict[str, Any] | None
    is_error: bool

    def rendered(self) -> str:
        """The result as a model receives it: prose plus the machine report."""
        if self.structured is None:
            return self.text
        payload = json.dumps(self.structured, separators=(",", ":"))
        return f"{self.text}\n{payload}" if self.text else payload


@dataclass
class Ledger:
    """Everything the sidecar handed back, bucketed by what it is.

    Kept separate rather than summed because the buckets answer different questions. `instructions`
    and `schemas` are the **fixed** grounding cost paid on every single turn — the number that
    creeps when someone adds a paragraph to a tool description. `resources` is opt-in grounding the
    model chose to read. `results` scales with how much work the task took.

    `schemas_bytes` and `tool_count` are the same schema payload measured two ways, kept so a report
    can derive `schema_density` — the number that tells roster GROWTH (more tools, flat density)
    apart from schema BLOAT (a wordier schema, no new tool). #612.
    """

    instructions: int = 0
    schemas: int = 0
    schemas_bytes: int = 0
    tool_count: int = 0
    resources: int = 0
    results: int = 0
    resource_uris: list[str] = field(default_factory=list)

    @property
    def fixed(self) -> int:
        """Grounding every turn pays before the model does anything: instructions + tool schemas."""
        return self.instructions + self.schemas

    @property
    def total(self) -> int:
        return self.instructions + self.schemas + self.resources + self.results

    @property
    def schema_density(self) -> float:
        """Schema bytes per tool. Growth-by-capability holds this flat; bloat pushes it up. 0.0 on
        an empty roster (no tools => no meaningful ratio)."""
        return self.schemas_bytes / self.tool_count if self.tool_count else 0.0

    def as_dict(self) -> dict[str, Any]:
        return {
            "instructions": self.instructions,
            "schemas": self.schemas,
            "schemas_bytes": self.schemas_bytes,
            "tool_count": self.tool_count,
            "schema_density": self.schema_density,
            "resources": self.resources,
            "results": self.results,
            "fixed": self.fixed,
            "total": self.total,
            "resource_uris": sorted(set(self.resource_uris)),
        }


def sidecar_binary() -> Path:
    """Locate the built `reuben-mcp`.

    Deliberately does NOT build it: a gate job that silently `cargo build`s hides a compile failure
    inside a token count. CI builds it as an explicit step.
    """
    override = os.environ.get("REUBEN_MCP_BIN")
    if override:
        path = Path(override)
        if not path.is_file():
            raise SidecarError(f"REUBEN_MCP_BIN={override} is not a file")
        return path
    for profile in ("release", "debug"):
        candidate = REPO / "target" / profile / "reuben-mcp"
        if candidate.is_file():
            return candidate
    found = shutil.which("reuben-mcp")
    if found:
        return Path(found)
    raise SidecarError(
        "reuben-mcp not found — run `cargo build -p reuben-mcp` or set REUBEN_MCP_BIN"
    )


class Sidecar:
    """One `reuben-mcp` process, spoken to over newline-delimited JSON-RPC.

    Used as a context manager; the process is killed on exit either way. `cwd` roots the sidecar's
    path resolution, so each task gets its own workspace and tasks cannot contaminate each other.
    """

    def __init__(self, cwd: Path) -> None:
        self._cwd = cwd
        self._proc: subprocess.Popen[str] | None = None
        self._next_id = 0
        self.ledger = Ledger()
        self.tools: dict[str, dict[str, Any]] = {}
        self.instructions = ""

    def __enter__(self) -> Sidecar:
        self._proc = subprocess.Popen(
            [str(sidecar_binary())],
            cwd=str(self._cwd),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
        # If the handshake fails, `__enter__` raises and Python never calls `__exit__`, so the child
        # and its pipes would leak. Reap it here before propagating.
        try:
            self._handshake()
        except BaseException:
            self.__exit__(None, None, None)
            raise
        return self

    def __exit__(self, *exc: object) -> None:
        proc = self._proc
        self._proc = None
        if proc is None:
            return
        try:
            # Closing stdin is EOF, which the sidecar treats as shutdown after draining.
            if proc.stdin:
                proc.stdin.close()
            proc.wait(timeout=5)
        except Exception:
            proc.kill()
            proc.wait(timeout=5)
        finally:
            if proc.stdout:
                proc.stdout.close()

    # -- wire ---------------------------------------------------------------------------------

    def _request(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        proc = self._proc
        if proc is None or proc.stdin is None or proc.stdout is None:
            raise SidecarError("sidecar is not running")
        self._next_id += 1
        request_id = self._next_id
        payload = {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params or {}}
        proc.stdin.write(json.dumps(payload) + "\n")
        proc.stdin.flush()

        # A watchdog rather than a blocking readline: a sidecar that wedges must fail the run with a
        # message, not sit until the job times out with nothing to show.
        box: list[str | None] = []

        def read() -> None:
            while True:
                line = proc.stdout.readline()  # type: ignore[union-attr]
                if not line:
                    box.append(None)
                    return
                if not line.strip():
                    continue
                message = json.loads(line)
                # Notifications and unrelated ids are skipped; we only await our own response.
                if message.get("id") == request_id:
                    box.append(line)
                    return

        thread = threading.Thread(target=read, daemon=True)
        thread.start()
        thread.join(TIMEOUT_SECONDS)
        if thread.is_alive():
            raise SidecarError(f"`{method}` did not answer within {TIMEOUT_SECONDS:.0f}s")
        if not box or box[0] is None:
            raise SidecarError(f"sidecar closed stdout while answering `{method}`")

        message = json.loads(box[0])
        if "error" in message:
            raise SidecarError(f"`{method}` returned a JSON-RPC error: {message['error']}")
        return message.get("result", {})

    def _notify(self, method: str) -> None:
        proc = self._proc
        if proc is None or proc.stdin is None:
            raise SidecarError("sidecar is not running")
        proc.stdin.write(json.dumps({"jsonrpc": "2.0", "method": method}) + "\n")
        proc.stdin.flush()

    def _handshake(self) -> None:
        result = self._request(
            "initialize",
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "reuben-eval", "version": "0"},
            },
        )
        self._notify("notifications/initialized")

        self.instructions = result.get("instructions") or ""
        self.ledger.instructions = cl100k.count(self.instructions)

        # `tools/list` is charged as grounding, not as a result: every client fetches it once before
        # the model speaks, and its size is what a wordier tool description actually costs.
        listed = self._request("tools/list").get("tools", [])
        self.tools = {tool["name"]: tool for tool in listed}
        schema_json = json.dumps(listed, separators=(",", ":"), sort_keys=True)
        self.ledger.schemas = cl100k.count(schema_json)
        # The same payload in bytes, plus the roster size: the pair a report divides into
        # per-tool density, so a wordier schema reads differently from one more verb. #612.
        self.ledger.schemas_bytes = len(schema_json.encode("utf-8"))
        self.ledger.tool_count = len(listed)

    # -- surface ------------------------------------------------------------------------------

    def openai_tools(self) -> list[dict[str, Any]]:
        """The sidecar's roster as OpenAI-compatible function definitions.

        A pure re-shaping of what `tools/list` advertised — names, descriptions and schemas pass
        through verbatim, so the live tier grounds the model on the same bytes the gate counts.
        """
        return [
            {
                "type": "function",
                "function": {
                    "name": tool["name"],
                    "description": tool.get("description", ""),
                    "parameters": tool.get("inputSchema", {"type": "object", "properties": {}}),
                },
            }
            for tool in self.tools.values()
        ]

    def read_resource(self, uri: str) -> str:
        result = self._request("resources/read", {"uri": uri})
        text = "".join(item.get("text", "") for item in result.get("contents", []))
        self.ledger.resources += cl100k.count(text)
        self.ledger.resource_uris.append(uri)
        return text

    def call_tool(self, name: str, arguments: dict[str, Any]) -> ToolResult:
        """Call a tool and price exactly what a model is fed by the result.

        The sidecar answers with BOTH a human `content` string and a `structuredContent` object —
        `validate` renders "invalid: 1 error(s), 0 warning(s)" alongside the machine report — and a
        client feeds both to the model. So both are tokenized, via `ToolResult.rendered()`, which is
        the very string the live tier hands the model (`runner.py`). What is NOT counted is the MCP
        transport framing around them — the `content`/`type`/`text`/`isError` envelope keys and the
        JSON-RPC wrapper — because no client shows a model those key names, and counting them would
        overstate the grounding budget (a real `validate` result: 58 envelope tokens vs 40 the model
        actually reads). Metric (a) is the model's grounding cost, so it must equal what the model is
        fed, in both tiers.
        """
        result = self._request("tools/call", {"name": name, "arguments": arguments})
        text = "".join(
            item.get("text", "") for item in result.get("content", []) if item.get("type") == "text"
        )
        structured = result.get("structuredContent")
        answer = ToolResult(
            text=text,
            structured=structured if isinstance(structured, dict) else None,
            is_error=bool(result.get("isError")),
        )
        self.ledger.results += cl100k.count(answer.rendered())
        return answer

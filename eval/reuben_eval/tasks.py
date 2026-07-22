"""The four task shapes, their reference solutions, and their structural assertions.

The shapes are frozen by #592: from-scratch construction, single-value tweak, intent-word nudge,
repair-from-broken. Each is bound to a committed `instruments/` fixture where one fits, so the
workload moves with the engine rather than rotting in a private copy.

**Pass is `validate` clean AND a structural assertion.** `validate` owns legality — the harness
never re-implements it (`#loader-single-authority`) — and the assertion owns "did the asked-for
thing actually happen". Both are needed: `scaffold_instrument` already emits a valid document, so
"change nothing" would otherwise score as success on the from-scratch task.

The assertions are deliberately strict about *collateral damage*. A single-value tweak that also
drops the document's `doc` prose is a failure, not a pass — that damage is exactly what
whole-document re-emission (`#whole-document-edit`) causes, and a metric blind to it would let the
thing this map is chasing pass unnoticed.
"""

from __future__ import annotations

import copy
import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable

REPO = Path(__file__).resolve().parent.parent.parent
FIXTURES = REPO / "instruments"

# Every task answers in this one file, so the harness always knows where to look for the result.
DOCUMENT = "instrument.json"


@dataclass(frozen=True)
class Step:
    """One call in a reference solution.

    `surface` is `host` for the client's own file tools and `mcp` for the sidecar's roster; a
    `resource` step reads a grounding document over `resources/read`.
    """

    surface: str
    name: str
    arguments: dict[str, Any] = field(default_factory=dict)


@dataclass(frozen=True)
class Task:
    key: str
    shape: str
    prompt: str
    seed: dict[str, str]
    reference: list[Step]
    assertion: Callable[[dict[str, Any]], None]
    document: str = DOCUMENT


# -- assertion helpers ------------------------------------------------------------------------


def _nodes(document: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {node["address"]: node for node in document.get("nodes", []) if "address" in node}


def _source_address(reference: Any) -> str | None:
    """The node address a wiring reference points at: `/env.cv` -> `/env`, `220.0` -> None."""
    if isinstance(reference, dict) and isinstance(reference.get("from"), str):
        return reference["from"].split(".", 1)[0]
    return None


def _upstream(document: dict[str, Any], address: str) -> set[str]:
    """Every node address reachable by walking `from` edges backwards out of `address`."""
    nodes = _nodes(document)
    seen: set[str] = set()
    frontier = [address]
    while frontier:
        current = frontier.pop()
        for value in (nodes.get(current) or {}).get("inputs", {}).values():
            source = _source_address(value)
            if source and source not in seen:
                seen.add(source)
                frontier.append(source)
    return seen


def assert_reaches_output(document: dict[str, Any], generator_type: str) -> None:
    """A node of `generator_type` must actually feed an `output` node.

    The reachability check #577 proposes folding into `validate`. Until it lands there, the harness
    asserts it independently — a disconnected oscillator is legal today and silent in practice.
    """
    nodes = _nodes(document)
    outputs = [address for address, node in nodes.items() if node.get("type") == "output"]
    if not outputs:
        raise AssertionError("no `output` node in the document")
    for output in outputs:
        for address in _upstream(document, output):
            if nodes.get(address, {}).get("type") == generator_type:
                return
    raise AssertionError(f"no `{generator_type}` node reaches an `output` node")


def assert_only_changed(
    original: dict[str, Any], produced: dict[str, Any], address: str, port: str
) -> Any:
    """Assert `produced` differs from `original` in exactly one node input, and return its value.

    Everything else — the `doc` prose, sibling nodes, the interface, unrelated ports — must survive
    byte-for-byte after parsing. Formatting and key order are free; content is not.
    """
    nodes = _nodes(produced)
    if address not in nodes:
        raise AssertionError(f"node `{address}` is missing from the produced document")
    value = nodes[address].get("inputs", {}).get(port)
    if value is None:
        raise AssertionError(f"`{address}` has no `{port}` input in the produced document")

    # Rebuild the original with only that one port replaced; anything else that moved shows up as a
    # mismatch, whatever nesting level it hides at.
    expected = copy.deepcopy(original)
    for node in expected.get("nodes", []):
        if node.get("address") == address:
            node.setdefault("inputs", {})[port] = value
    if json.loads(json.dumps(expected, sort_keys=True)) != json.loads(
        json.dumps(produced, sort_keys=True)
    ):
        raise AssertionError(
            f"the edit changed more than `{address}.{port}` — collateral damage from re-emitting "
            "the whole document"
        )
    return value


# -- fixtures ---------------------------------------------------------------------------------


def _fixture(relative: str) -> str:
    return (FIXTURES / relative).read_text(encoding="utf-8")


VOICE = _fixture("voices/default-voice.json")
VOICE_DOCUMENT: dict[str, Any] = json.loads(VOICE)


def _broken_voice() -> str:
    """`default-voice` with one dangling edge — a real fixture with one real defect.

    A typo'd source address is the most common repair a model actually meets, and `validate`
    reports it precisely ("reference to unknown node"), so the repair is deterministic rather than
    a matter of taste.
    """
    document = copy.deepcopy(VOICE_DOCUMENT)
    for node in document["nodes"]:
        if node["address"] == "/env_vca":
            node["inputs"]["b"] = {"from": "/env_curv"}
    return json.dumps(document, indent=2) + "\n"


BROKEN = _broken_voice()
ORIGINAL_CUTOFF = float(_nodes(VOICE_DOCUMENT)["/filter"]["inputs"]["cutoff"])


# -- the four tasks ---------------------------------------------------------------------------


def _from_scratch_document() -> dict[str, Any]:
    return {
        "format_version": 3,
        "instrument": "tone",
        "interface": {"outputs": {"out": {"from": "/out.audio"}}},
        "nodes": [
            {"type": "oscillator", "address": "/osc", "inputs": {"freq": 220.0}},
            {
                "type": "filter",
                "address": "/filter",
                "inputs": {"audio": {"from": "/osc"}, "cutoff": 1200.0},
            },
            {"type": "output", "address": "/out", "inputs": {"audio": {"from": "/filter"}}},
        ],
    }


def _assert_from_scratch(document: dict[str, Any]) -> None:
    types = {node.get("type") for node in document.get("nodes", [])}
    for required in ("oscillator", "filter", "output"):
        if required not in types:
            raise AssertionError(f"no `{required}` node in the document")
    assert_reaches_output(document, "oscillator")


def _assert_tweak(document: dict[str, Any]) -> None:
    value = assert_only_changed(VOICE_DOCUMENT, document, "/filter", "cutoff")
    if not isinstance(value, (int, float)) or float(value) != 800.0:
        raise AssertionError(f"`/filter.cutoff` is {value!r}, expected 800")


def _assert_nudge(document: dict[str, Any]) -> None:
    value = assert_only_changed(VOICE_DOCUMENT, document, "/filter", "cutoff")
    if not isinstance(value, (int, float)):
        raise AssertionError(f"`/filter.cutoff` is {value!r}, expected a number")
    # `warmer` is filter.cutoff *down* per the intent vocabulary. Direction is the assertion; the
    # size of the step is #575's question, not this harness's.
    if not 0.0 < float(value) < ORIGINAL_CUTOFF:
        raise AssertionError(
            f"`warmer` must lower `/filter.cutoff` below {ORIGINAL_CUTOFF}; got {value}"
        )


def _assert_repair(document: dict[str, Any]) -> None:
    nodes = _nodes(document)
    if len(nodes) != len(_nodes(VOICE_DOCUMENT)):
        raise AssertionError(
            f"the repair changed the node count ({len(nodes)} vs {len(_nodes(VOICE_DOCUMENT))}) — "
            "deleting the node is not fixing the edge"
        )
    source = _source_address((nodes.get("/env_vca") or {}).get("inputs", {}).get("b"))
    if source is None:
        raise AssertionError("`/env_vca.b` is no longer wired from a node")
    if source not in nodes:
        raise AssertionError(f"`/env_vca.b` still dangles at `{source}`")
    assert_reaches_output(document, "oscillator")


TASKS: list[Task] = [
    Task(
        key="from_scratch",
        shape="from-scratch construction",
        prompt=(
            "Create a new instrument document at `instrument.json`. It should be a simple tone: an "
            "oscillator running through a lowpass filter into the output. Make sure it validates."
        ),
        seed={},
        reference=[
            Step("mcp", "scaffold_instrument", {"name": "tone"}),
            Step(
                "host",
                "write_file",
                {"path": DOCUMENT, "content": json.dumps(_from_scratch_document(), indent=2) + "\n"},
            ),
            Step("mcp", "validate", {"path": DOCUMENT}),
        ],
        assertion=_assert_from_scratch,
    ),
    Task(
        key="tweak",
        shape="single-value tweak",
        prompt=(
            "In `instrument.json`, set the filter's cutoff to 800. Change nothing else. Make sure "
            "it still validates."
        ),
        seed={DOCUMENT: VOICE},
        reference=[
            Step("host", "read_file", {"path": DOCUMENT}),
            # The document payload is filled in by `_finish_reference_solutions` below.
            Step("host", "write_file", {"path": DOCUMENT, "content": ""}),
            Step("mcp", "validate", {"path": DOCUMENT}),
        ],
        assertion=_assert_tweak,
    ),
    Task(
        key="nudge",
        shape="intent-word nudge",
        prompt=(
            "Make the instrument in `instrument.json` warmer. Apply the project's intent vocabulary "
            "and keep everything else as it is. Make sure it still validates."
        ),
        seed={DOCUMENT: VOICE},
        reference=[
            Step("resource", "read", {"uri": "reuben://guide/vocabulary"}),
            Step("host", "read_file", {"path": DOCUMENT}),
            Step("host", "write_file", {"path": DOCUMENT, "content": ""}),
            Step("mcp", "validate", {"path": DOCUMENT}),
        ],
        assertion=_assert_nudge,
    ),
    Task(
        key="repair",
        shape="repair-from-broken",
        prompt=(
            "`instrument.json` does not load. Find out why and fix it, keeping every node that is "
            "there now."
        ),
        seed={DOCUMENT: BROKEN},
        reference=[
            Step("mcp", "validate", {"path": DOCUMENT}),
            Step("host", "read_file", {"path": DOCUMENT}),
            Step("host", "write_file", {"path": DOCUMENT, "content": ""}),
            Step("mcp", "validate", {"path": DOCUMENT}),
        ],
        assertion=_assert_repair,
    ),
]


def _finish_reference_solutions() -> None:
    """Fill in the whole-document payloads the tweak/nudge/repair references must emit.

    Written here rather than inline so each reference is unmistakably *the ideal sequence*: read
    once, emit the corrected document once, validate by path. That is the surface's cost floor, and
    metric (c) prices it at one full document — which is exactly the number #576 and #583 exist to
    move.
    """
    tweaked = copy.deepcopy(VOICE_DOCUMENT)
    _nodes(tweaked)["/filter"]["inputs"]["cutoff"] = 800.0

    warmed = copy.deepcopy(VOICE_DOCUMENT)
    _nodes(warmed)["/filter"]["inputs"]["cutoff"] = 2000.0

    repaired = copy.deepcopy(json.loads(BROKEN))
    _nodes(repaired)["/env_vca"]["inputs"]["b"] = {"from": "/env_curve"}

    payloads = {"tweak": tweaked, "nudge": warmed, "repair": repaired}
    for task in TASKS:
        document = payloads.get(task.key)
        if document is None:
            continue
        for index, step in enumerate(task.reference):
            if step.name == "write_file":
                task.reference[index] = Step(
                    step.surface,
                    step.name,
                    {"path": DOCUMENT, "content": json.dumps(document, indent=2) + "\n"},
                )


_finish_reference_solutions()

BY_KEY = {task.key: task for task in TASKS}

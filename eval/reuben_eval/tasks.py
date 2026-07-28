"""The task shapes, their reference solutions, and their structural assertions.

The first four shapes are frozen: from-scratch construction, single-value tweak, intent-word
application, repair-from-broken. The fifth is the same intent-word shape at fan-out, on a document
big enough for one word to stand in for nine edits — a new series, so nothing on the frozen four
moves. Each is bound to a committed `instruments/` fixture where one fits, so the workload moves
with the engine rather than rotting in a private copy.

**Pass is `validate_instrument` clean AND a structural assertion.** It owns legality — the harness
never re-implements it (`#loader-single-authority`) — and the assertion owns "did the asked-for
thing actually happen". Both are needed: `new_instrument` already lands a valid document, so
"change nothing" would otherwise score as success on the from-scratch task.

The assertions are deliberately strict about *collateral damage*. A single-value tweak that also
drops the document's `doc` prose is a failure, not a pass — that damage is exactly what
whole-document re-emission causes (see rules: agent-mcp), and a metric blind to it would let the
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

    The reachability check is proposed for folding into `validate`. Until it lands there, the harness
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

    # A document verb migrates `format_version` on write, and several committed fixtures are still
    # on an older one — so the field moves under any model that edits through the roster rather than
    # re-emitting the file. That is the engine's edit, not the model's, and whether the result is
    # legal is `validate_instrument`'s to say, not this assertion's (see rules: agent-mcp).
    expected.pop("format_version", None)
    produced = {key: entry for key, entry in produced.items() if key != "format_version"}

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

    A typo'd source address is the most common repair a model actually meets, and `validate_instrument`
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

def _nested_seed(relative: str) -> dict[str, str]:
    """Every document `relative` reaches by reference, keyed as the workspace must hold it.

    References resolve sibling-first from the referring document's directory, then the library root
    — so this walks the same two places the engine's resolver does, and keys each hit by its path
    under `instruments/` so the seeded tree has the shape the committed library has.
    """
    seed: dict[str, str] = {}
    frontier = [relative]
    while frontier:
        current = frontier.pop()
        document = json.loads((FIXTURES / current).read_text(encoding="utf-8"))
        directory = Path(current).parent
        for source in document.get("resources", {}).values():
            for candidate in (directory / source, Path(source)):
                if (FIXTURES / candidate).is_file():
                    key = candidate.as_posix()
                    if key not in seed:
                        seed[key] = _fixture(key)
                        frontier.append(key)
                    break
    return dict(sorted(seed.items()))


# The fan-out fixture: 53 nodes, and the library's densest target set for a single intent word.
ACID = _fixture("acid-techno.json")
ACID_DOCUMENT: dict[str, Any] = json.loads(ACID)
# `acid-techno` nests five voice instruments, one of which nests again. The whole tree rides along
# in the seed so the workspace copy loads exactly as the committed library does — a fixture that
# only half resolves would measure a pile of dark-resource warnings instead of the surface.
ACID_SEED: dict[str, str] = _nested_seed("acid-techno.json")
ORIGINAL_GLIDE = float(ACID_DOCUMENT["interface"]["inputs"]["glide"]["default"])


# -- the tasks --------------------------------------------------------------------------------


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


def _assert_intent_word(document: dict[str, Any]) -> None:
    value = assert_only_changed(VOICE_DOCUMENT, document, "/filter", "cutoff")
    if not isinstance(value, (int, float)):
        raise AssertionError(f"`/filter.cutoff` is {value!r}, expected a number")
    # `warmer` is filter.cutoff *down* per the intent vocabulary. Direction is the assertion; the
    # size of the step is the intent vocabulary's question, not this harness's.
    if not 0.0 < float(value) < ORIGINAL_CUTOFF:
        raise AssertionError(
            f"`warmer` must lower `/filter.cutoff` below {ORIGINAL_CUTOFF}; got {value}"
        )


def _assert_intent_fan_out(document: dict[str, Any]) -> None:
    """`looser` on `acid-techno`: nine targets, one of them reached through a wire.

    Spelled out rather than counted from the vocabulary, because the fan-out *is* the claim — one
    word standing in for nine `set_instrument_input` calls plus a range lookup each — and an
    assertion that re-derived the targets from the same table would agree with any bug in it.
    """
    original, produced = _nodes(ACID_DOCUMENT), _nodes(document)
    expected = copy.deepcopy(ACID_DOCUMENT)
    moved: list[str] = []

    def moved_up(address: str, port: str) -> None:
        before = float(original[address]["inputs"][port])
        after = produced.get(address, {}).get("inputs", {}).get(port)
        if not isinstance(after, (int, float)) or float(after) <= before:
            raise AssertionError(
                f"`looser` must raise `{address}.{port}` above {before}; got {after!r}"
            )
        moved.append(f"{address}.{port}")
        for node in expected["nodes"]:
            if node["address"] == address:
                node["inputs"][port] = after

    # `looser` is envelope.attack up (slightly) plus m2s.time up.
    for address, node in original.items():
        if node["type"] == "envelope":
            moved_up(address, "attack")
        elif node["type"] == "m2s" and not isinstance(node["inputs"].get("time"), dict):
            moved_up(address, "time")

    # The wired target: `/bass_glide.time` is fed by the `glide` interface pipe, so the pipe's own
    # value is what moves — and the wire is still there afterwards.
    if produced["/bass_glide"]["inputs"]["time"] != {"from": "/glide"}:
        raise AssertionError("`looser` severed `/bass_glide.time` instead of following it")
    glide = document.get("interface", {}).get("inputs", {}).get("glide", {}).get("default")
    if not isinstance(glide, (int, float)) or float(glide) <= ORIGINAL_GLIDE:
        raise AssertionError(
            f"`looser` must raise the `glide` pipe's value above {ORIGINAL_GLIDE}; got {glide!r}"
        )
    expected["interface"]["inputs"]["glide"]["default"] = glide
    moved.append("/glide.in")

    if len(moved) != 9:
        raise AssertionError(f"`looser` on acid-techno is nine targets; moved {len(moved)}: {moved}")

    # Same collateral-damage bar the single-value tasks hold, over nine slots instead of one. The
    # `format_version` caveat on `assert_only_changed` applies here for the same reason.
    expected.pop("format_version", None)
    rest = {key: entry for key, entry in document.items() if key != "format_version"}
    if json.loads(json.dumps(expected, sort_keys=True)) != json.loads(
        json.dumps(rest, sort_keys=True)
    ):
        raise AssertionError("the batch changed more than the nine targets `looser` names")


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
            Step("mcp", "new_instrument", {"source": DOCUMENT, "name": "tone"}),
            Step(
                "host",
                "write_file",
                {"path": DOCUMENT, "content": json.dumps(_from_scratch_document(), indent=2) + "\n"},
            ),
            Step("mcp", "validate_instrument", {"source": DOCUMENT}),
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
            Step("mcp", "validate_instrument", {"source": DOCUMENT}),
        ],
        assertion=_assert_tweak,
    ),
    Task(
        key="intent_word",
        shape="intent-word application",
        prompt=(
            "Make the instrument in `instrument.json` warmer. Apply the project's intent vocabulary "
            "and keep everything else as it is. Make sure it still validates."
        ),
        seed={DOCUMENT: VOICE},
        # One call: the word goes in, the engine does the row → nodes → arithmetic join and
        # re-validates before writing. No vocabulary read, no document read, no re-emission — the
        # three things this shape used to cost. It exercises **one** target (`warmer` matches one of
        # its three moves on this fixture); `intent_fan_out` below is where the fan-out shows.
        reference=[
            Step(
                "mcp",
                "set_instrument_inputs_by_intent",
                {"source": DOCUMENT, "word": "warmer"},
            ),
        ],
        assertion=_assert_intent_word,
    ),
    Task(
        key="intent_fan_out",
        shape="intent-word fan-out",
        prompt=(
            "Make the instrument in `instrument.json` looser. Apply the project's intent vocabulary "
            "everywhere it applies and keep everything else as it is. Make sure it still validates."
        ),
        seed={DOCUMENT: ACID, **ACID_SEED},
        reference=[
            Step(
                "mcp",
                "set_instrument_inputs_by_intent",
                {"source": DOCUMENT, "word": "looser"},
            ),
        ],
        assertion=_assert_intent_fan_out,
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
            Step("mcp", "validate_instrument", {"source": DOCUMENT}),
            Step("host", "read_file", {"path": DOCUMENT}),
            Step("host", "write_file", {"path": DOCUMENT, "content": ""}),
            Step("mcp", "validate_instrument", {"source": DOCUMENT}),
        ],
        assertion=_assert_repair,
    ),
]


def _finish_reference_solutions() -> None:
    """Fill in the whole-document payloads the tweak/repair references must emit.

    Written here rather than inline so each reference is unmistakably *the ideal sequence*: read
    once, emit the corrected document once, validate by source. That is the surface's cost floor, and
    metric (c) prices it at one full document — which is exactly the number the surface
    work exists to move. The intent-word references emit nothing at all, so they are not here.
    """
    tweaked = copy.deepcopy(VOICE_DOCUMENT)
    _nodes(tweaked)["/filter"]["inputs"]["cutoff"] = 800.0

    repaired = copy.deepcopy(json.loads(BROKEN))
    _nodes(repaired)["/env_vca"]["inputs"]["b"] = {"from": "/env_curve"}

    payloads = {"tweak": tweaked, "repair": repaired}
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

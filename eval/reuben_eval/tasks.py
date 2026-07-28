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
drops the document's `doc` prose is a failure, not a pass (see rules: agent-mcp), and a metric blind
to it would let the thing this map is chasing pass unnoticed. Two different mechanisms produce that
damage — re-emitting the whole document, and a second, unasked-for verb call — so the failure names
*what* moved and leaves the cause to the trace.
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

# Bumped whenever a reference solution is rewritten. A reference is the ideal call sequence, so
# rewriting one is a deliberate step change in that task's trend series rather than a surface
# regression — the gate and the history record carry this number so the reader who meets the
# discontinuity is told which it was instead of inferring the wrong one.
#
# 1: the whole-document procedure — read the file, re-emit the corrected document, validate by path.
# 2: the document verbs — a one-value edit is one `set_instrument_input` call.
REFERENCE_REVISION = 2
REFERENCE_REVISION_NOTE = (
    "the reference solutions were rewritten to the document verbs, so a step in these series is the "
    "harness asking for a different ideal call sequence — not the surface getting dearer or cheaper"
)


@dataclass(frozen=True)
class Step:
    """One call in a reference solution.

    `surface` is `mcp` for the sidecar's roster and `host` for a tool the client brings itself; a
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


def _changed_addresses(expected: dict[str, Any], produced: dict[str, Any]) -> list[str]:
    """Name every place `produced` differs from `expected`, the way the document addresses it.

    Reports *what* moved, never why: re-emitting the whole document and a second, unasked-for verb
    call both land here, and a message that picked one would send the reader to the wrong place.
    """
    changes: list[str] = []
    for key in sorted(set(expected) | set(produced)):
        if key == "nodes" or expected.get(key) == produced.get(key):
            continue
        if key not in produced:
            changes.append(f"{key} (dropped)")
        elif key not in expected:
            changes.append(f"{key} (added)")
        else:
            changes.append(key)

    before, after = _nodes(expected), _nodes(produced)
    for node_address in sorted(set(before) | set(after)):
        if node_address not in after:
            changes.append(f"{node_address} (dropped)")
            continue
        if node_address not in before:
            changes.append(f"{node_address} (added)")
            continue
        was, now = before[node_address], after[node_address]
        for attribute in sorted(set(was) | set(now)):
            if attribute == "inputs":
                was_inputs, now_inputs = was.get("inputs", {}), now.get("inputs", {})
                changes += [
                    f"{node_address}.{name}"
                    for name in sorted(set(was_inputs) | set(now_inputs))
                    if was_inputs.get(name) != now_inputs.get(name)
                ]
            elif was.get(attribute) != now.get(attribute):
                changes.append(f"{node_address}.{attribute}")
    return changes


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
        moved = _changed_addresses(expected, produced)
        detail = ", ".join(moved) if moved else "the node order"
        raise AssertionError(f"the edit changed more than `{address}.{port}`: also changed {detail}")
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
    """The document the `from_scratch` reference assembles, node for node and pipe for pipe.

    Kept beside the call sequence that builds it so the rewrite from whole-document emission to the
    verbs changed the *procedure* without changing the artifact — the series stays comparable across
    the step. `tests/test_tasks.py` holds the two in lockstep.
    """
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


def _from_scratch_reference() -> list[Step]:
    """Build `_from_scratch_document()` one verb at a time — the ideal from-scratch sequence.

    Derived from the target document rather than transcribed beside it, so the two cannot drift into
    a procedure that no longer produces the artifact. `new_instrument` lands the valid seed; each
    `add_instrument_node` carries the node's literals and wires in the same call; the output pipe is
    the master tap. There is no closing `validate_instrument` — a verb re-validates the whole
    document and writes only if it is valid, so the last one already answered the question.
    """
    document = _from_scratch_document()
    steps = [Step("mcp", "new_instrument", {"source": DOCUMENT, "name": document["instrument"]})]
    steps += [
        Step(
            "mcp",
            "add_instrument_node",
            {
                "source": DOCUMENT,
                "address": node["address"],
                "type": node["type"],
                "inputs": node["inputs"],
            },
        )
        for node in document["nodes"]
    ]
    steps += [
        Step(
            "mcp",
            "add_instrument_interface_output",
            {"source": DOCUMENT, "name": name, "from": pipe["from"]},
        )
        for name, pipe in document["interface"]["outputs"].items()
    ]
    return steps


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
        reference=_from_scratch_reference(),
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
        # The whole task, in one call. The address and the port are named in the prompt, so nothing
        # has to be read first, and the verb re-validates before it writes — which is the "make sure
        # it still validates" half.
        reference=[
            Step(
                "mcp",
                "set_instrument_input",
                {"source": DOCUMENT, "address": "/filter", "input": "cutoff", "value": 800.0},
            ),
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
        # Validate is the diagnosis — it names the dangling reference `/env_curv` but not the node
        # that was meant, so the index projection is the read that supplies `/env_curve`. Then one
        # `wire_instrument_input` re-points the edge; a verb reaches a document that does not load,
        # which is what makes repair expressible without touching its bytes.
        reference=[
            Step("mcp", "validate_instrument", {"source": DOCUMENT}),
            Step("mcp", "describe_instrument", {"source": DOCUMENT, "view": "index"}),
            Step(
                "mcp",
                "wire_instrument_input",
                {
                    "source": DOCUMENT,
                    "address": "/env_vca",
                    "input": "b",
                    "from": "/env_curve",
                },
            ),
        ],
        assertion=_assert_repair,
    ),
]


BY_KEY = {task.key: task for task in TASKS}

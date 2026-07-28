"""The harness's forcing function: prove the assertions actually reject the degenerate passes.

A structural assertion that never fails is worse than no assertion — it reports a green ladder while
measuring nothing. The specific trap this harness was built around: `new_instrument` already lands
a valid document, so "change nothing" would score as success on the from-scratch task unless
something checks the asked-for thing happened.

So every test here is the *negative*: feed the assertion a document that a lazy or damaging model
would plausibly produce, and require it to raise.

Needs a built sidecar for the reference-solution test (`cargo build -p reuben-mcp`); that one test
skips when the binary is absent so the rest still run on a bare checkout.
"""

from __future__ import annotations

import copy
import json
import pathlib
import tempfile
import unittest

from reuben_eval import tasks
from reuben_eval.mcp import Sidecar, SidecarError, sidecar_binary
from reuben_eval.runner import Session, run_reference
from reuben_eval.workspace import (
    FILE_ACCESS,
    HOST_TOOLS,
    PayloadLedger,
    file_access_failure,
    looks_like_file_access,
)


def sidecar_available() -> bool:
    try:
        sidecar_binary()
        return True
    except SidecarError:
        return False


def _replay(task, extra_calls=()):
    """Replay a reference (plus any extra calls) and hand back the outcome and the document.

    `run_reference` deliberately reports only the numbers; a test that has to inspect what was
    actually written needs the workspace alive at scoring time, which is what this holds open.
    """
    with tempfile.TemporaryDirectory(prefix=f"reuben-eval-{task.key}-") as root:
        with Session(task, pathlib.Path(root) / "workspace") as session:
            for step in task.reference:
                if step.surface == "resource":
                    session.read_resource(str(step.arguments["uri"]))
                else:
                    session.call(step.name, dict(step.arguments))
            for name, arguments in extra_calls:
                session.call(name, dict(arguments))
            return session.judge(), session.workspace.read_document(task.document)


class TestReferenceSolutions(unittest.TestCase):
    """The floor must be reachable, or the gate is measuring a broken workload."""

    @unittest.skipUnless(sidecar_available(), "reuben-mcp not built")
    def test_every_reference_solution_passes(self) -> None:
        for task in tasks.TASKS:
            with self.subTest(task=task.key):
                outcome = run_reference(task)
                self.assertTrue(outcome.passed, f"{task.key}: {outcome.failure}")

    @unittest.skipUnless(sidecar_available(), "reuben-mcp not built")
    def test_repair_task_costs_exactly_one_repair_round(self) -> None:
        """Metric (b)'s floor for `repair` is 1 — its first `validate` IS the diagnosis."""
        self.assertEqual(run_reference(tasks.BY_KEY["repair"]).repair_rounds, 1)

    @unittest.skipUnless(sidecar_available(), "reuben-mcp not built")
    def test_engine_actually_rejects_the_broken_fixture(self) -> None:
        """The measured surface must still catch the defect the repair task is built on.

        `test_repair_task_costs_exactly_one_repair_round` already fails if this regresses (the round
        would drop 1→0), but only indirectly and with a puzzling message. Assert it head-on: if the
        engine ever stops rejecting the dangling edge, the repair task silently becomes a no-op and
        its cost drop reads to the gate as an improvement. This is the explicit tripwire.
        """
        from reuben_eval.mcp import Sidecar

        with tempfile.TemporaryDirectory(prefix="reuben-broken-") as root:
            path = pathlib.Path(root) / tasks.DOCUMENT
            path.write_text(tasks.BROKEN, encoding="utf-8")
            with Sidecar(pathlib.Path(root)) as sidecar:
                verdict = sidecar.call_tool("validate_instrument", {"source": tasks.DOCUMENT})
        self.assertIs(
            (verdict.structured or {}).get("ok"),
            False,
            "the engine no longer rejects the BROKEN fixture — the repair task is measuring nothing",
        )

    @unittest.skipUnless(sidecar_available(), "reuben-mcp not built")
    def test_the_tweak_floor_emits_no_document_at_all(self) -> None:
        """The number the surface work existed to move, arrived: a one-value change is one call.

        It used to be a whole document. `set_instrument_input` names the address, the port and the
        value and nothing else, so metric (c) prices the ideal tweak at zero.
        """
        outcome = run_reference(tasks.BY_KEY["tweak"])
        self.assertEqual(outcome.payload_characters, 0)
        self.assertEqual(len(tasks.BY_KEY["tweak"].reference), 1)

    @unittest.skipUnless(sidecar_available(), "reuben-mcp not built")
    def test_the_from_scratch_reference_builds_the_document_it_declares(self) -> None:
        """The verb sequence and `_from_scratch_document` must not drift apart.

        Asserted on the *produced* document rather than on the steps, because a step-by-step
        comparison only ever checks the parts it thinks to walk: add a `doc` to the target and a
        nodes-and-pipes comparison stays green while the document silently loses it. This is the
        claim the docstring makes — the rewrite changed the procedure and not the artifact — so it
        is the claim that has to be tested.
        """
        _, produced = _replay(tasks.BY_KEY["from_scratch"])
        self.assertEqual(produced, tasks._from_scratch_document())


class TestFromScratchAssertion(unittest.TestCase):
    def test_scaffold_alone_is_not_a_pass(self) -> None:
        """The degenerate pass: a valid but empty document — what
        `new_instrument` lands, unchanged."""
        with self.assertRaises(AssertionError):
            tasks._assert_from_scratch({"format_version": 3, "instrument": "tone", "nodes": []})

    def test_disconnected_oscillator_is_not_a_pass(self) -> None:
        """Every node present, none of them wired — legal today, silent in practice."""
        document = {
            "format_version": 3,
            "instrument": "tone",
            "nodes": [
                {"type": "oscillator", "address": "/osc"},
                {"type": "filter", "address": "/filter"},
                {"type": "output", "address": "/out"},
            ],
        }
        with self.assertRaises(AssertionError):
            tasks._assert_from_scratch(document)

    def test_the_reference_document_passes(self) -> None:
        tasks._assert_from_scratch(tasks._from_scratch_document())


class TestTweakAssertion(unittest.TestCase):
    def _tweaked(self, cutoff: float) -> dict:
        document = copy.deepcopy(tasks.VOICE_DOCUMENT)
        tasks._nodes(document)["/filter"]["inputs"]["cutoff"] = cutoff
        return document

    def test_unchanged_document_fails(self) -> None:
        with self.assertRaises(AssertionError):
            tasks._assert_tweak(copy.deepcopy(tasks.VOICE_DOCUMENT))

    def test_correct_tweak_passes(self) -> None:
        tasks._assert_tweak(self._tweaked(800.0))

    def test_dropping_the_doc_prose_fails(self) -> None:
        """Collateral damage is a failure, not a pass — and the failure names what moved.

        Two mechanisms produce it: a whole-document re-emit, and a second, unasked-for verb call.
        Naming a cause would send the reader to the wrong one half the time, so the message reports
        the address instead and leaves the cause to the trace.
        """
        document = self._tweaked(800.0)
        document.pop("doc")
        with self.assertRaises(AssertionError) as caught:
            tasks._assert_tweak(document)
        self.assertIn("doc (dropped)", str(caught.exception))

    def test_a_stray_second_edit_names_the_address_it_touched(self) -> None:
        """The live-tier shape: the ideal one-call edit, plus one volunteered node description."""
        document = self._tweaked(800.0)
        tasks._nodes(document)["/filter"]["doc"] = "a gentler lowpass"
        with self.assertRaises(AssertionError) as caught:
            tasks._assert_tweak(document)
        self.assertIn("/filter.doc", str(caught.exception))

    def test_losing_a_sibling_node_fails(self) -> None:
        document = self._tweaked(800.0)
        document["nodes"] = [n for n in document["nodes"] if n["address"] != "/env_curve"]
        with self.assertRaises(AssertionError):
            tasks._assert_tweak(document)

    def test_reformatting_alone_is_fine(self) -> None:
        """Key order and whitespace are free; content is not."""
        document = json.loads(json.dumps(self._tweaked(800.0), sort_keys=True))
        tasks._assert_tweak(document)

    def test_the_format_migration_is_not_collateral_damage(self) -> None:
        """A document verb upgrades `format_version` on write, and the fixture is still on 2.

        That edit is the engine's, not the model's. Counting it as damage made the single-value and
        intent-word tasks unpassable by anything that used the document verbs at all.
        """
        document = self._tweaked(800.0)
        document["format_version"] = tasks.VOICE_DOCUMENT["format_version"] + 1
        tasks._assert_tweak(document)


class TestIntentWordAssertion(unittest.TestCase):
    def _with_cutoff(self, cutoff: float) -> dict:
        document = copy.deepcopy(tasks.VOICE_DOCUMENT)
        tasks._nodes(document)["/filter"]["inputs"]["cutoff"] = cutoff
        return document

    def test_warmer_lowers_the_cutoff(self) -> None:
        tasks._assert_intent_word(self._with_cutoff(2000.0))

    def test_wrong_direction_fails(self) -> None:
        """`warmer` is cutoff *down*; raising it is `brighter`, the opposite move."""
        with self.assertRaises(AssertionError):
            tasks._assert_intent_word(self._with_cutoff(6000.0))

    def test_no_change_fails(self) -> None:
        with self.assertRaises(AssertionError):
            tasks._assert_intent_word(self._with_cutoff(tasks.ORIGINAL_CUTOFF))

    def test_zeroing_the_filter_fails(self) -> None:
        """"Warmer" is a step, not a mute — a degenerate floor is not a pass."""
        with self.assertRaises(AssertionError):
            tasks._assert_intent_word(self._with_cutoff(0.0))

    def test_the_format_migration_is_not_collateral_damage(self) -> None:
        document = self._with_cutoff(2000.0)
        document["format_version"] = tasks.VOICE_DOCUMENT["format_version"] + 1
        tasks._assert_intent_word(document)


class TestRepairAssertion(unittest.TestCase):
    def _repaired(self, source: str) -> dict:
        document = json.loads(tasks.BROKEN)
        tasks._nodes(document)["/env_vca"]["inputs"]["b"] = {"from": source}
        return document

    def test_rewiring_to_the_real_node_passes(self) -> None:
        tasks._assert_repair(self._repaired("/env_curve"))

    def test_still_dangling_fails(self) -> None:
        with self.assertRaises(AssertionError):
            tasks._assert_repair(json.loads(tasks.BROKEN))

    def test_deleting_the_node_is_not_a_repair(self) -> None:
        """`validate` would go clean — which is exactly why legality alone can't score this."""
        document = json.loads(tasks.BROKEN)
        document["nodes"] = [n for n in document["nodes"] if n["address"] != "/env_vca"]
        for node in document["nodes"]:
            if node["address"] == "/out":
                node["inputs"]["audio"] = {"from": "/filter"}
        with self.assertRaises(AssertionError):
            tasks._assert_repair(document)

    def test_the_broken_fixture_is_actually_broken(self) -> None:
        """Guard against a fixture that silently stops carrying its defect."""
        self.assertNotEqual(json.loads(tasks.BROKEN), tasks.VOICE_DOCUMENT)
        source = tasks._source_address(
            tasks._nodes(json.loads(tasks.BROKEN))["/env_vca"]["inputs"]["b"]
        )
        self.assertNotIn(source, tasks._nodes(tasks.VOICE_DOCUMENT))


class TestPayloadLedger(unittest.TestCase):
    """Metric (c): echoes count, small structured arguments cost nothing.

    `write_file` is off the roster, so every charge here is an *invented* call — which is exactly the
    case the ledger still has to price. A model that emits a document at a tool that refuses it spent
    the characters; the refusal does not refund them.
    """

    def test_echoes_are_charged(self) -> None:
        # An echo is a model writing a document it has already emitted once — the re-emit this
        # metric exists to kill. Both writes are charged: the second is not free for being a repeat.
        ledger = PayloadLedger()
        for _ in range(2):
            ledger.charge("write_file", {"path": "a.json", "content": tasks.VOICE},
                          on_roster=False)
        self.assertGreater(ledger.characters, len(tasks.VOICE))
        self.assertEqual(set(ledger.per_tool), {"write_file"})

    def test_a_document_is_charged_whatever_the_invented_call_calls_it(self) -> None:
        """The argument name is as free as the encoding, so a list of names prices nothing.

        These are not hypothetical spellings: `patch` is OpenAI Codex's `apply_patch`, `file_text`
        is Anthropic's text editor. Any enumeration is one agent product behind the next one
        shipped, so the value is read instead of its label. A false zero is the one direction this
        metric must never be fooled in, now that it is a floor and a tripwire rather than a spread.
        """
        for tool, argument in (
            ("write_file", "content"),
            ("writeFile", "content"),
            ("Write", "content"),
            ("write_file", "text"),
            ("write_document", "document"),
            ("str_replace_editor", "file_text"),
            ("save_file", "body"),
            ("apply_patch", "patch"),
            ("edit_file", "diff"),
            ("create_file", "code"),
            ("upload", "payload"),
            ("submit", "instrument"),
            ("post", "json"),
        ):
            with self.subTest(tool=tool, argument=argument):
                ledger = PayloadLedger()
                ledger.charge(tool, {"path": "a.json", argument: tasks.VOICE}, on_roster=False)
                self.assertEqual(ledger.characters, len(tasks.VOICE))

    def test_a_small_document_is_charged_even_under_the_size_floor(self) -> None:
        """Size does most of the work; the document keys catch what slips under it."""
        seed = json.dumps({"format_version": 3, "instrument": "tone", "nodes": []})
        self.assertLess(len(seed), 200)
        ledger = PayloadLedger()
        ledger.charge("apply_patch", {"patch": seed}, on_roster=False)
        self.assertEqual(ledger.characters, len(seed))

    def test_a_refused_call_that_emits_nothing_costs_nothing(self) -> None:
        """A shell reach is still a reach, but no document crossed — classification, not payload."""
        ledger = PayloadLedger()
        ledger.charge("bash", {"command": "cat instrument.json"}, on_roster=False)
        ledger.charge("read_file", {"path": "instrument.json"}, on_roster=False)
        ledger.charge("Read", {"file_path": "instrument.json", "limit": 200}, on_roster=False)
        self.assertEqual(ledger.characters, 0)

    def test_intent_sized_arguments_cost_nothing(self) -> None:
        """A word, a node address and a float are what this map wants the model emitting."""
        ledger = PayloadLedger()
        ledger.charge("validate_instrument", {"source": "instrument.json"}, on_roster=True)
        ledger.charge(
            "send_live_controls",
            {"messages": [{"address": "/filt/cutoff", "args": [800.0]}]},
            on_roster=True,
        )
        ledger.charge("new_instrument", {"source": "instrument.json", "name": "tone"},
                      on_roster=True)
        ledger.charge("set_instrument_input", {"source": "instrument.json",
                                               "address": "/filter", "input": "cutoff",
                                               "value": 800.0}, on_roster=True)
        self.assertEqual(ledger.characters, 0)

    def test_a_verb_argument_is_not_a_document(self) -> None:
        """A node's inputs map is structured, and still free. The metric prices freehand JSON.

        `add_instrument_node(inputs=…)` is small, schema-named, and is the thing the verbs exist to
        make cheap — charging it would price the cure as the disease. The roster half of the ledger
        is name-keyed precisely so a verb argument can never be caught by argument shape.
        """
        ledger = PayloadLedger()
        ledger.charge(
            "add_instrument_node",
            {"source": "instrument.json", "address": "/filter", "type": "filter",
             "inputs": {"audio": {"from": "/osc"}, "cutoff": 1200.0}},
            on_roster=True,
        )
        self.assertEqual(ledger.characters, 0)

    def test_encoding_does_not_change_the_price(self) -> None:
        """A document costs the same whether emitted as a JSON string or a parsed object.

        A model inventing the call is not bound by any schema, so either encoding can arrive. Pricing
        one cheaper would make the wrong move look like the cheap one.
        """
        compact = json.dumps(tasks.VOICE_DOCUMENT, separators=(",", ":"))
        as_string = PayloadLedger()
        as_string.charge("write_file", {"content": compact}, on_roster=False)
        as_object = PayloadLedger()
        as_object.charge("write_file", {"content": tasks.VOICE_DOCUMENT}, on_roster=False)
        self.assertEqual(as_string.characters, as_object.characters)


class TestIntentFanOutAssertion(unittest.TestCase):
    """`looser` on `acid-techno`: the shape where one word replaces nine edits."""

    def _loosened(self) -> dict:
        """The document the intent verb produces — every target raised, the wire intact."""
        document = copy.deepcopy(tasks.ACID_DOCUMENT)
        for node in document["nodes"]:
            if node["type"] == "envelope":
                node["inputs"]["attack"] = float(node["inputs"]["attack"]) * 1.2
            elif node["type"] == "m2s" and not isinstance(node["inputs"].get("time"), dict):
                node["inputs"]["time"] = float(node["inputs"]["time"]) * 1.5
        document["interface"]["inputs"]["glide"]["default"] = tasks.ORIGINAL_GLIDE + 0.125
        return document

    def test_moving_every_target_passes(self) -> None:
        tasks._assert_intent_fan_out(self._loosened())

    def test_moving_only_some_of_them_fails(self) -> None:
        """A batch that stopped early is the failure mode a per-node assertion exists to catch."""
        document = self._loosened()
        tasks._nodes(document)["/kick_trim"]["inputs"]["time"] = 0.02
        with self.assertRaises(AssertionError):
            tasks._assert_intent_fan_out(document)

    def test_severing_the_wire_fails(self) -> None:
        """A literal over `/bass_glide.time` unplugs the instrument's own glide control."""
        document = self._loosened()
        tasks._nodes(document)["/bass_glide"]["inputs"]["time"] = 0.2
        with self.assertRaises(AssertionError):
            tasks._assert_intent_fan_out(document)

    def test_leaving_the_pipe_alone_fails(self) -> None:
        document = self._loosened()
        document["interface"]["inputs"]["glide"]["default"] = tasks.ORIGINAL_GLIDE
        with self.assertRaises(AssertionError):
            tasks._assert_intent_fan_out(document)

    def test_collateral_damage_fails(self) -> None:
        document = self._loosened()
        document["nodes"] = [n for n in document["nodes"] if n["address"] != "/dsat"]
        with self.assertRaises(AssertionError):
            tasks._assert_intent_fan_out(document)


class TestTaskRoster(unittest.TestCase):
    def test_the_shapes_are_all_present(self) -> None:
        """The shapes are frozen; losing one silently narrows what the gate can see."""
        self.assertEqual(
            {task.key for task in tasks.TASKS},
            {"from_scratch", "tweak", "intent_word", "intent_fan_out", "repair"},
        )

    @unittest.skipUnless(sidecar_available(), "reuben-mcp not built")
    def test_every_reference_solution_actually_writes_the_document(self) -> None:
        """A reference must leave the document changed, not merely mention it.

        Asserted on the result rather than on the call list. Matching step arguments for the answer
        document's name cannot tell a write from a read — `validate_instrument` and
        `describe_instrument` both name their `source` — so such a check stays green on a reference
        whose only mutating verb has been deleted. Running it is the only way to know.
        """
        for task in tasks.TASKS:
            with self.subTest(task=task.key):
                _, produced = _replay(task)
                seed = task.seed.get(task.document)
                if seed is not None:
                    self.assertNotEqual(
                        produced, json.loads(seed), "the reference left the document untouched"
                    )


class TestFileAccessIsANamedFailure(unittest.TestCase):
    """`file-access`: an agent tried to read or write a file.

    A conforming client has no reason to touch instrument JSON — the projection is the read, the
    document verbs are the write — so the harness does not merely omit file tools, it fails the
    **reach** for one. Detecting the attempt rather than a completed operation is what keeps this a
    live signal after the tools are gone: reuben cannot take `Read`/`Write` away from a real host, so
    what this measures is whether a model still wants the old path when the surface stops offering
    it. A check that cannot be made to fail is not a check, so these fire it.
    """

    # Tools a real host actually offers, not names built backwards from the matcher. Claude Code's
    # own roster, Anthropic's text-editor tool, the canonical MCP filesystem server, and the shells a
    # model falls back to when none of those are there. Every one of these is a path by which a model
    # that lost `write_file` can still emit a whole document.
    REAL_HOST_TOOLS = (
        # Claude Code, and Anthropic's text editor across its spellings.
        "Read", "Write", "Edit", "Glob", "Bash", "BashTool",
        "str_replace_editor", "text_editor", "str_replace_based_edit_tool",
        # The canonical MCP filesystem server.
        "read_file", "write_file", "read_text_file", "move_file", "search_files",
        "get_file_info", "directory_tree", "list_directory", "create_directory",
        # The shell each major agent harness actually ships — the category words (`bash`, `shell`)
        # are not what any of them is called.
        "execute_command", "run_shell_command", "run_terminal_cmd", "execute_bash", "local_shell",
        "shell_exec", "container_exec", "run_command", "subprocess", "zsh", "sh",
        "code_interpreter", "python",
        # Assorted spellings a model reaches for unprompted.
        "readFile", "writeFile", "apply_patch", "write_document", "save_document", "view", "cat",
    )

    def test_the_model_facing_roster_offers_nothing_that_trips_it(self) -> None:
        for tool in HOST_TOOLS:
            with self.subTest(tool=tool["function"]["name"]):
                self.assertFalse(looks_like_file_access(tool["function"]["name"]))

    def test_read_guide_never_trips_it(self) -> None:
        """Grounding prose is meant for the model's context; this decision does not touch it."""
        self.assertIn("read_guide", {tool["function"]["name"] for tool in HOST_TOOLS})
        self.assertFalse(looks_like_file_access("read_guide"))
        self.assertIsNone(file_access_failure([]))

    def test_every_real_host_file_tool_is_classified(self) -> None:
        """The names that matter are the ones a model reaches for, not the two reuben retired.

        An AND of action-word and filesystem-noun is a *stricter* gate than the two literal names it
        replaced: `Read`, `Write` and `Edit` are bare verbs with no noun, and `bash` has neither
        half. Missing those lets a model fall back to the retired path and still be reported as
        having stopped wanting it — the exact false green the removal was supposed to rule out.
        """
        for name in self.REAL_HOST_TOOLS:
            with self.subTest(tool=name):
                self.assertTrue(looks_like_file_access(name))

    def test_the_message_names_what_was_reached_for(self) -> None:
        message = file_access_failure(["Write", "read_file"])
        self.assertIn(FILE_ACCESS, message)
        self.assertIn("read or write a file", message)
        self.assertIn("Write", message)
        self.assertIn("read_file", message)

    def test_it_does_not_catch_a_document_verb(self) -> None:
        """A near-miss on a real verb is a malformed call, not a reach for the filesystem."""
        for name in ("describe_instrument", "set_instrument_input", "new_instrument",
                     "add_instrument_node", "wire_instrument_input", "send_live_controls",
                     "swap_instrument", "validate_instrument", "describe_operators"):
            with self.subTest(tool=name):
                self.assertFalse(looks_like_file_access(name))

    @unittest.skipUnless(sidecar_available(), "reuben-mcp not built")
    def test_no_name_on_the_live_roster_trips_it(self) -> None:
        """The static sample above is a sample; this is the whole roster, whatever it grows to."""
        with tempfile.TemporaryDirectory(prefix="reuben-roster-") as root:
            with Sidecar(pathlib.Path(root)) as sidecar:
                names = sorted(sidecar.tools)
        self.assertGreater(len(names), 20, "the roster did not load")
        for name in names:
            with self.subTest(tool=name):
                self.assertFalse(looks_like_file_access(name))

    def test_no_reference_solution_reaches_for_a_file(self) -> None:
        for task in tasks.TASKS:
            with self.subTest(task=task.key):
                reached = [step.name for step in task.reference if looks_like_file_access(step.name)]
                self.assertIsNone(file_access_failure(reached), file_access_failure(reached))

    @unittest.skipUnless(sidecar_available(), "reuben-mcp not built")
    def test_a_reach_after_a_correct_edit_still_fails_the_run(self) -> None:
        """Driven end to end, through the same path a live model's call takes.

        The `tweak` is correct and the document validates — only the reach failed the run. That is
        the point: the harness scores what the model wanted, not just what it left behind.
        """
        outcome, produced = _replay(
            tasks.BY_KEY["tweak"], [("read_file", {"path": tasks.DOCUMENT})]
        )
        tasks._assert_tweak(produced)  # the document is right
        self.assertFalse(outcome.passed)  # the run is not
        self.assertEqual(outcome.failure_mode, FILE_ACCESS)
        self.assertIn("read_file", outcome.failure)


if __name__ == "__main__":
    unittest.main()

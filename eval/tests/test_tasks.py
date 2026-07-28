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
from reuben_eval.mcp import SidecarError, sidecar_binary
from reuben_eval.runner import Session, run_reference
from reuben_eval.workspace import (
    FILE_ACCESS,
    FILE_TOOLS,
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

    def test_the_from_scratch_reference_builds_the_document_it_declares(self) -> None:
        """The verb sequence and `_from_scratch_document` must not drift apart."""
        document = tasks._from_scratch_document()
        added = [
            step.arguments
            for step in tasks.BY_KEY["from_scratch"].reference
            if step.name == "add_instrument_node"
        ]
        self.assertEqual(
            [(a["address"], a["type"], a["inputs"]) for a in added],
            [(n["address"], n["type"], n["inputs"]) for n in document["nodes"]],
        )


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
        ledger.charge("write_file", {"path": "a.json", "content": tasks.VOICE})
        ledger.charge("write_file", {"path": "a.json", "content": tasks.VOICE})
        self.assertGreater(ledger.characters, len(tasks.VOICE))
        self.assertEqual(set(ledger.per_tool), {"write_file"})

    def test_intent_sized_arguments_cost_nothing(self) -> None:
        """A word, a node address and a float are what this map wants the model emitting."""
        ledger = PayloadLedger()
        ledger.charge("validate_instrument", {"source": "instrument.json"})
        ledger.charge(
            "send_live_controls", {"messages": [{"address": "/filt/cutoff", "args": [800.0]}]}
        )
        ledger.charge("new_instrument", {"source": "instrument.json", "name": "tone"})
        ledger.charge("set_instrument_input", {"source": "instrument.json",
                                               "address": "/filter", "port": "cutoff",
                                               "value": 800.0})
        self.assertEqual(ledger.characters, 0)

    def test_encoding_does_not_change_the_price(self) -> None:
        """A document costs the same whether emitted as a JSON string or a parsed object.

        A model inventing the call is not bound by any schema, so either encoding can arrive. Pricing
        one cheaper would make the wrong move look like the cheap one.
        """
        compact = json.dumps(tasks.VOICE_DOCUMENT, separators=(",", ":"))
        as_string = PayloadLedger()
        as_string.charge("write_file", {"content": compact})
        as_object = PayloadLedger()
        as_object.charge("write_file", {"content": tasks.VOICE_DOCUMENT})
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

    def test_every_task_has_a_reference_solution_that_writes_the_document(self) -> None:
        """A reference must produce the answer document — through a verb, or by emitting it."""
        for task in tasks.TASKS:
            with self.subTest(task=task.key):
                writes = [
                    step
                    for step in task.reference
                    if step.arguments.get("source") == task.document
                    or step.arguments.get("path") == task.document
                ]
                self.assertTrue(writes, "a reference solution must produce the answer document")


class TestFileAccessIsANamedFailure(unittest.TestCase):
    """`file-access`: an agent tried to read or write a file.

    A conforming client has no reason to touch instrument JSON — the projection is the read, the
    document verbs are the write — so the harness does not merely omit file tools, it fails the
    **reach** for one. Detecting the attempt rather than a completed operation is what keeps this a
    live signal after the tools are gone: reuben cannot take `Read`/`Write` away from a real host, so
    what this measures is whether a model still wants the old path when the surface stops offering
    it. A check that cannot be made to fail is not a check, so these fire it.
    """

    def test_the_model_facing_roster_has_no_file_tool(self) -> None:
        offered = {tool["function"]["name"] for tool in HOST_TOOLS}
        self.assertEqual(offered & set(FILE_TOOLS), set())

    def test_read_guide_never_trips_it(self) -> None:
        """Grounding prose is meant for the model's context; this decision does not touch it."""
        self.assertIn("read_guide", {tool["function"]["name"] for tool in HOST_TOOLS})
        self.assertFalse(looks_like_file_access("read_guide"))
        self.assertIsNone(file_access_failure([]))

    def test_the_retired_names_are_classified(self) -> None:
        for name in FILE_TOOLS:
            with self.subTest(tool=name):
                self.assertTrue(looks_like_file_access(name))
                message = file_access_failure([name])
                self.assertIn(FILE_ACCESS, message)
                self.assertIn("read or write a file", message)
                self.assertIn(name, message)

    def test_the_shape_survives_the_names_going_away(self) -> None:
        """What a model emits once `read_file` is gone is whatever its priors call the same move."""
        for name in ("readFile", "fs_write", "open_file", "list_files", "save-file", "edit_path"):
            with self.subTest(tool=name):
                self.assertTrue(looks_like_file_access(name))

    def test_it_does_not_catch_a_document_verb(self) -> None:
        """A near-miss on a real verb is a malformed call, not a reach for the filesystem."""
        for name in ("describe_instrument", "set_instrument_input", "new_instrument", "swap"):
            with self.subTest(tool=name):
                self.assertFalse(looks_like_file_access(name))

    def test_no_reference_solution_reaches_for_a_file(self) -> None:
        for task in tasks.TASKS:
            with self.subTest(task=task.key):
                reached = [step.name for step in task.reference if looks_like_file_access(step.name)]
                self.assertIsNone(file_access_failure(reached), file_access_failure(reached))

    @unittest.skipUnless(sidecar_available(), "reuben-mcp not built")
    def test_a_run_that_emits_read_file_fails_with_the_named_mode(self) -> None:
        """The check driven end to end, through the same path a live model's call takes."""
        task = tasks.BY_KEY["tweak"]
        with tempfile.TemporaryDirectory(prefix="reuben-eval-reach-") as root:
            with Session(task, pathlib.Path(root) / "workspace") as session:
                for step in task.reference:
                    session.call(step.name, dict(step.arguments))
                session.call("read_file", {"path": tasks.DOCUMENT})
                outcome = session.judge()
        # The document is correct — only the reach failed the run, which is the point.
        self.assertFalse(outcome.passed)
        self.assertEqual(outcome.failure_mode, FILE_ACCESS)
        self.assertIn("read_file", outcome.failure)


if __name__ == "__main__":
    unittest.main()

"""The harness's forcing function: prove the assertions actually reject the degenerate passes.

A structural assertion that never fails is worse than no assertion — it reports a green ladder while
measuring nothing. #592 named the specific trap: `scaffold_instrument` already emits a valid
document, so "change nothing" would score as success on the from-scratch task unless something
checks the asked-for thing happened.

So every test here is the *negative*: feed the assertion a document that a lazy or damaging model
would plausibly produce, and require it to raise.

Needs a built sidecar for the reference-solution test (`cargo build -p reuben-mcp`); that one test
skips when the binary is absent so the rest still run on a bare checkout.
"""

from __future__ import annotations

import copy
import json
import unittest

from reuben_eval import tasks
from reuben_eval.mcp import SidecarError, sidecar_binary
from reuben_eval.runner import run_reference
from reuben_eval.workspace import PayloadLedger


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
    def test_tweak_floor_re_emits_the_whole_document(self) -> None:
        """The number #576 and #583 exist to move: a one-value change costs a full document."""
        outcome = run_reference(tasks.BY_KEY["tweak"])
        self.assertGreater(
            outcome.payload_characters,
            0.9 * len(tasks.VOICE),
            "the tweak floor should be roughly one whole document; if this dropped, a point-edit "
            "path landed and the baseline moved on purpose",
        )


class TestFromScratchAssertion(unittest.TestCase):
    def test_scaffold_alone_is_not_a_pass(self) -> None:
        """The degenerate pass #592 called out: a valid but empty document."""
        with self.assertRaises(AssertionError):
            tasks._assert_from_scratch({"format_version": 3, "instrument": "tone", "nodes": []})

    def test_disconnected_oscillator_is_not_a_pass(self) -> None:
        """Every node present, none of them wired — legal today, silent in practice (#577)."""
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
        """Collateral damage from re-emitting the whole document is a failure, not a pass."""
        document = self._tweaked(800.0)
        document.pop("doc")
        with self.assertRaises(AssertionError):
            tasks._assert_tweak(document)

    def test_losing_a_sibling_node_fails(self) -> None:
        document = self._tweaked(800.0)
        document["nodes"] = [n for n in document["nodes"] if n["address"] != "/env_curve"]
        with self.assertRaises(AssertionError):
            tasks._assert_tweak(document)

    def test_reformatting_alone_is_fine(self) -> None:
        """Key order and whitespace are free; content is not."""
        document = json.loads(json.dumps(self._tweaked(800.0), sort_keys=True))
        tasks._assert_tweak(document)


class TestNudgeAssertion(unittest.TestCase):
    def _with_cutoff(self, cutoff: float) -> dict:
        document = copy.deepcopy(tasks.VOICE_DOCUMENT)
        tasks._nodes(document)["/filter"]["inputs"]["cutoff"] = cutoff
        return document

    def test_warmer_lowers_the_cutoff(self) -> None:
        tasks._assert_nudge(self._with_cutoff(2000.0))

    def test_wrong_direction_fails(self) -> None:
        """`warmer` is cutoff *down*; raising it is `brighter`, the opposite move."""
        with self.assertRaises(AssertionError):
            tasks._assert_nudge(self._with_cutoff(6000.0))

    def test_no_change_fails(self) -> None:
        with self.assertRaises(AssertionError):
            tasks._assert_nudge(self._with_cutoff(tasks.ORIGINAL_CUTOFF))

    def test_zeroing_the_filter_fails(self) -> None:
        """"Warmer" is a nudge, not a mute — a degenerate floor is not a pass."""
        with self.assertRaises(AssertionError):
            tasks._assert_nudge(self._with_cutoff(0.0))


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
    """Metric (c): echoes count, small structured arguments cost nothing."""

    def test_echoes_are_charged(self) -> None:
        ledger = PayloadLedger()
        document = tasks.VOICE_DOCUMENT
        ledger.charge("write_file", {"path": "a.json", "content": tasks.VOICE})
        ledger.charge("validate", {"document": document})
        self.assertGreater(ledger.characters, len(tasks.VOICE))
        self.assertEqual(set(ledger.per_tool), {"write_file", "validate"})

    def test_intent_sized_arguments_cost_nothing(self) -> None:
        """A word, a node address and a float are what this map wants the model emitting."""
        ledger = PayloadLedger()
        ledger.charge("validate", {"path": "instrument.json"})
        ledger.charge("send", {"messages": [{"address": "/filt/cutoff", "args": [800.0]}]})
        ledger.charge("scaffold_instrument", {"name": "tone"})
        self.assertEqual(ledger.characters, 0)

    def test_encoding_does_not_change_the_price(self) -> None:
        """A document costs the same whether emitted as a JSON string or a parsed object."""
        compact = json.dumps(tasks.VOICE_DOCUMENT, separators=(",", ":"))
        as_string = PayloadLedger()
        as_string.charge("write_file", {"content": compact})
        as_object = PayloadLedger()
        as_object.charge("validate", {"document": tasks.VOICE_DOCUMENT})
        self.assertEqual(as_string.characters, as_object.characters)


class TestTaskRoster(unittest.TestCase):
    def test_the_four_shapes_are_all_present(self) -> None:
        """#592 froze the shapes; losing one silently narrows what the gate can see."""
        self.assertEqual(
            {task.key for task in tasks.TASKS}, {"from_scratch", "tweak", "nudge", "repair"}
        )

    def test_every_task_has_a_reference_solution_that_writes_the_document(self) -> None:
        for task in tasks.TASKS:
            with self.subTest(task=task.key):
                writes = [step for step in task.reference if step.name == "write_file"]
                self.assertTrue(writes, "a reference solution must produce the answer document")
                for step in writes:
                    self.assertTrue(
                        step.arguments["content"].strip(),
                        "reference payloads are filled in by _finish_reference_solutions",
                    )


if __name__ == "__main__":
    unittest.main()

"""Tests for the control-surface generator (ADR-0018). Run: python3 -m unittest -v
(from this directory, or `python3 -m unittest discover .claude/skills/control-surface`).

These cover the deterministic half — metadata resolution, candidate inference, OSC addressing,
and the zlib/XML round-trip. Whether the emitted .tosc *loads in TouchOSC* is the on-device
verify step the skill calls out; it cannot be asserted here."""

import unittest
import zlib
from pathlib import Path
from xml.etree import ElementTree as ET

import gen_surface as g

# A known-good TouchOSC export (built by hand in the editor) — the ground truth the emitter is
# cloned from. The structural-match test below fails if our output drifts from this format.
FIXTURE = Path(__file__).parent / "fixtures" / "REUBEN_REF.tosc"

# A tiny schema in the committed shape: one map (the Good Button workhorse) + a clock param.
SCHEMA = {
    "$defs": {"node": {"allOf": [
        {"if": {"properties": {"type": {"const": "map"}}},
         "then": {"properties": {"params": {"properties": {
             "in_min": {"minimum": -1e6, "maximum": 1e6, "default": 0.0, "description": "unit: , curve: Linear"},
             "in_max": {"minimum": -1e6, "maximum": 1e6, "default": 1.0, "description": "unit: , curve: Linear"},
             "default": {"minimum": -1e6, "maximum": 1e6, "default": 0.0, "description": "unit: , curve: Linear"},
         }}}}},
        {"if": {"properties": {"type": {"const": "clock"}}},
         "then": {"properties": {"params": {"properties": {
             "tempo": {"minimum": 1.0, "maximum": 999.0, "default": 120.0, "description": "unit: BPM, curve: Linear"},
         }}}}},
        {"if": {"properties": {"type": {"const": "sequencer"}}},
         "then": {"properties": {"params": {"properties": {
             "step1": {"minimum": 0.0, "maximum": 1.0, "default": 0.0, "description": "unit: , curve: Linear"},
             "step2": {"minimum": 0.0, "maximum": 1.0, "default": 0.0, "description": "unit: , curve: Linear"},
         }}}}},
    ]}}
}

META = g.load_param_meta(SCHEMA)

# A Good Button map (public, no incoming connection), a ranged map fed by it (plumbing), and
# a clock with a tempo param.
INSTRUMENT = {
    "instrument": "t",
    "nodes": [
        {"type": "map", "address": "/brightness",
         "params": {"in_min": 0.0, "in_max": 100.0, "default": 50.0},
         "control": {"label": "Brightness", "unit": "%"}},
        {"type": "map", "address": "/map_cutoff", "params": {"out_min": 400, "out_max": 12000}},
        {"type": "clock", "address": "/clock",
         "control": [{"label": "Tempo", "param": "tempo"}]},
    ],
    "connections": [
        {"from": {"node": "/brightness", "port": "out"}, "to": {"node": "/map_cutoff", "port": "in"}},
    ],
}


class MetaTest(unittest.TestCase):
    def test_parses_unit_and_curve(self):
        self.assertEqual(META["clock"]["tempo"]["unit"], "BPM")
        self.assertEqual(META["map"]["in_max"]["default"], 1.0)


class InferTest(unittest.TestCase):
    def test_public_map_is_a_good_button(self):
        cands = g.infer_candidates(INSTRUMENT, META)
        addrs = [c["address"] for c in cands["good_buttons"]]
        self.assertIn("/brightness", addrs)

    def test_connected_map_is_excluded(self):
        cands = g.infer_candidates(INSTRUMENT, META)
        addrs = [c["address"] for c in cands["good_buttons"]]
        self.assertNotIn("/map_cutoff", addrs, "a fed map is plumbing, not a public control")

    def test_params_are_listed_with_metadata(self):
        cands = g.infer_candidates(INSTRUMENT, META)
        tempo = next(p for p in cands["params"] if p["address"] == "/clock/tempo")
        self.assertEqual((tempo["min"], tempo["max"], tempo["unit"]), (1.0, 999.0, "BPM"))


class ResolveTest(unittest.TestCase):
    def test_good_button_binds_to_node_address_with_instance_range(self):
        node = INSTRUMENT["nodes"][0]
        r = g.resolve_control(node, {"label": "Brightness", "unit": "%"}, META)
        self.assertEqual(r["osc_addr"], "/brightness")
        self.assertEqual((r["min"], r["max"], r["default"]), (0.0, 100.0, 50.0))
        self.assertEqual(r["unit"], "%")

    def test_param_binds_to_node_slash_param_from_schema(self):
        node = INSTRUMENT["nodes"][2]
        r = g.resolve_control(node, {"label": "Tempo", "param": "tempo"}, META)
        self.assertEqual(r["osc_addr"], "/clock/tempo")
        self.assertEqual((r["min"], r["max"], r["default"]), (1.0, 999.0, 120.0))
        self.assertEqual(r["unit"], "BPM")

    def test_spec_overrides_win(self):
        node = INSTRUMENT["nodes"][2]
        r = g.resolve_control(node, {"label": "T", "param": "tempo", "max": 200, "default": 90}, META)
        self.assertEqual((r["max"], r["default"]), (200.0, 90.0))

    def test_object_and_array_control_both_collected(self):
        controls = g.collect_controls(INSTRUMENT, META)
        addrs = sorted(c["osc_addr"] for c in controls)
        self.assertEqual(addrs, ["/brightness", "/clock/tempo"])


class NoteToggleTest(unittest.TestCase):
    NODE = {"type": "voicer", "address": "/voicer",
            "control": {"label": "Play C", "widget": "note-toggle", "port": "note", "note": 60}}

    def test_resolves_to_node_port_address_and_note(self):
        r = g.resolve_control(self.NODE, self.NODE["control"], META)
        self.assertEqual(r["kind"], "note-toggle")
        self.assertEqual(r["osc_addr"], "/voicer/note")
        self.assertEqual(r["note"], 60.0)

    def test_emits_a_toggle_button_with_constant_note_and_gate(self):
        c = g.resolve_control(self.NODE, self.NODE["control"], META)
        doc = ET.fromstring(zlib.decompress(g.build_tosc({"instrument": "t"}, [c], 1)))
        btn = doc.find(".//node[@type='BUTTON']")
        self.assertIsNotNone(btn, "note-toggle must emit a BUTTON")
        btype = [p for p in btn.findall("./properties/property") if p.find("key").text == "buttonType"][0]
        self.assertEqual(btype.find("value").text, "2", "buttonType 2 = Toggle Press")
        args = btn.findall("./messages/osc/arguments/partial")
        self.assertEqual(args[0].find("type").text, "CONSTANT")
        self.assertEqual(args[0].find("value").text, "60")   # the fixed MIDI note
        self.assertEqual(args[1].find("type").text, "VALUE")
        self.assertEqual(args[1].find("value").text, "x")    # the gate
        self.assertEqual(btn.find("./messages/osc/path/partial/value").text, "/voicer/note")


class ParamToggleTest(unittest.TestCase):
    # A gate-mode sequencer lane: stepN are boolean on/off (ADR-0022), each carrying a control.
    LANE = {"type": "sequencer", "address": "/kick",
            "params": {"gate_mode": 1.0, "length": 16.0, "step1": 1.0, "step2": 0.0},
            "control": [{"label": "K1", "param": "step1", "min": 0.0, "max": 1.0},
                        {"label": "K2", "param": "step2", "min": 0.0, "max": 1.0}]}

    def test_gate_step_autodetected_as_toggle(self):
        r = g.resolve_control(self.LANE, self.LANE["control"][0], META)
        self.assertEqual(r["kind"], "param-toggle")
        self.assertEqual(r["osc_addr"], "/kick/step1")
        self.assertEqual(r["node"], "/kick")
        self.assertEqual(r["default"], 1.0)   # mirrors the instrument's resting step value

    def test_continuous_sequencer_step_stays_a_fader(self):
        # gate_mode=0 -> stepN is a continuous degree, not a boolean; must remain a fader.
        node = dict(self.LANE, params={"gate_mode": 0.0})
        r = g.resolve_control(node, {"label": "K1", "param": "step1", "min": 0.0, "max": 1.0}, META)
        self.assertEqual(r["kind"], "fader")

    def test_emits_a_button_sending_x_to_the_param(self):
        c = g.resolve_control(self.LANE, self.LANE["control"][0], META)
        doc = ET.fromstring(zlib.decompress(g.build_tosc({"instrument": "t"}, [c], 4)))
        btn = doc.find(".//node[@type='BUTTON']")
        self.assertIsNotNone(btn, "param-toggle must emit a BUTTON")
        btype = [p for p in btn.findall("./properties/property") if p.find("key").text == "buttonType"][0]
        self.assertEqual(btype.find("value").text, "2", "buttonType 2 = Toggle Press")
        self.assertEqual(btn.find("./messages/osc/path/partial/value").text, "/kick/step1")
        args = btn.findall("./messages/osc/arguments/partial")
        self.assertEqual(len(args), 1, "the button's x IS the payload — no constant note")
        self.assertEqual(args[0].find("type").text, "VALUE")
        self.assertEqual(args[0].find("value").text, "x")

    def test_lane_packs_into_one_full_width_row(self):
        # 16 steps + a trailing fader: the steps share one row of 16; the fader drops below.
        controls = [g.resolve_control(
            {"type": "sequencer", "address": "/kick", "params": {"gate_mode": 1.0}},
            {"label": f"K{i}", "param": f"step{i}"}, META) for i in range(1, 17)]
        controls.append(g.resolve_control(INSTRUMENT["nodes"][2], {"label": "Tempo", "param": "tempo"}, META))
        rows = g.layout_rows(controls, cols=4)
        self.assertEqual(len(rows[0]), 16, "all 16 lane steps share one row")
        self.assertTrue(all(c["kind"] == "param-toggle" for c in rows[0]))
        self.assertEqual([c["label"] for c in rows[-1]], ["Tempo"], "non-toggle controls grid below")


class ChordButtonTest(unittest.TestCase):
    """The V1.3 chord buttons (ADR-0022): a toggle whose payload is a custom `[degree, gate]`,
    sent to the `chord` op's address. Same 2-arg button mechanism as note-toggle, but the
    constant is a scale degree (the chord root), not an absolute MIDI note."""

    NODE = {"type": "chord", "address": "/chord",
            "control": {"label": "IV", "widget": "chord-button", "port": "set", "degree": 3}}

    def test_resolves_to_node_port_address_and_degree(self):
        r = g.resolve_control(self.NODE, self.NODE["control"], META)
        self.assertEqual(r["kind"], "chord-button")
        self.assertEqual(r["osc_addr"], "/chord/set")
        self.assertEqual(r["degree"], 3.0)

    def test_port_defaults_to_set(self):
        node = {"type": "chord", "address": "/chord",
                "control": {"label": "I", "widget": "chord-button", "degree": 0}}
        r = g.resolve_control(node, node["control"], META)
        self.assertEqual(r["osc_addr"], "/chord/set")

    def test_emits_a_toggle_button_with_constant_degree_and_gate(self):
        c = g.resolve_control(self.NODE, self.NODE["control"], META)
        doc = ET.fromstring(zlib.decompress(g.build_tosc({"instrument": "t"}, [c], 1)))
        btn = doc.find(".//node[@type='BUTTON']")
        self.assertIsNotNone(btn, "chord-button must emit a BUTTON")
        btype = [p for p in btn.findall("./properties/property") if p.find("key").text == "buttonType"][0]
        self.assertEqual(btype.find("value").text, "2", "buttonType 2 = Toggle Press")
        args = btn.findall("./messages/osc/arguments/partial")
        self.assertEqual(args[0].find("type").text, "CONSTANT")
        self.assertEqual(args[0].find("value").text, "3")    # the fixed chord-root degree
        self.assertEqual(args[1].find("type").text, "VALUE")
        self.assertEqual(args[1].find("value").text, "x")    # the gate
        self.assertEqual(btn.find("./messages/osc/path/partial/value").text, "/chord/set")

    def test_seven_chord_buttons_each_send_their_own_degree(self):
        # The Chord-player surface: 7 buttons, root degrees 0..6 -> /chord/set.
        nodes = [{"type": "chord", "address": "/chord",
                  "control": [{"label": f"d{d}", "widget": "chord-button", "degree": d}
                              for d in range(7)]}]
        controls = g.collect_controls({"instrument": "cp", "nodes": nodes}, META)
        self.assertEqual(len(controls), 7)
        doc = ET.fromstring(zlib.decompress(g.build_tosc({"instrument": "cp"}, controls, 7)))
        degrees = []
        for btn in doc.findall(".//node[@type='BUTTON']"):
            addr = btn.find("./messages/osc/path/partial/value").text
            self.assertEqual(addr, "/chord/set", "all chord buttons hit one address")
            degrees.append(btn.findall("./messages/osc/arguments/partial")[0].find("value").text)
        self.assertEqual(degrees, ["0", "1", "2", "3", "4", "5", "6"])


class EmitTest(unittest.TestCase):
    def setUp(self):
        controls = g.collect_controls(INSTRUMENT, META)
        self.doc = ET.fromstring(zlib.decompress(g.build_tosc(INSTRUMENT, controls, 4)))

    def test_round_trips_to_parseable_xml(self):
        # Matches the editor's export: lexml version 6.
        self.assertEqual(self.doc.tag, "lexml")
        self.assertEqual(self.doc.get("version"), "6")

    def test_fader_addresses_and_scaling(self):
        # Partials use child elements (not attributes). The argument scaling maps x[0,1] to the
        # control's real range, e.g. /clock/tempo -> [1,999].
        found = {}
        for osc in self.doc.findall(".//osc"):
            addr = osc.find("./path/partial/value").text
            arg = osc.find("./arguments/partial")
            found[addr] = (arg.find("scaleMin").text, arg.find("scaleMax").text)
        self.assertEqual(found["/clock/tempo"], ("1", "999"))
        self.assertEqual(found["/brightness"], ("0", "100"))

    def test_label_text_lives_in_values(self):
        # The bug that made labels read "Label": text is a <values> entry keyed `text`, not a
        # property. Every LABEL must carry one.
        for label in self.doc.findall(".//node[@type='LABEL']"):
            keys = [v.find("key").text for v in label.findall("./values/value")]
            self.assertIn("text", keys)

    def test_send_only_one_way(self):
        osc = self.doc.find(".//osc")
        self.assertEqual(osc.find("send").text, "1")
        self.assertEqual(osc.find("receive").text, "0")

    def test_deterministic_bytes(self):
        controls = g.collect_controls(INSTRUMENT, META)
        a = g.build_tosc(INSTRUMENT, controls, 4)
        b = g.build_tosc(INSTRUMENT, controls, 4)
        self.assertEqual(a, b, "same instrument must emit identical bytes")


class FixtureMatchTest(unittest.TestCase):
    """Lock the emitter to the known-good export: for each control type, our property *key set*
    must match the reference's. Catches format drift (a renamed/missing property) that unit
    tests on our own output alone would miss."""

    @classmethod
    def setUpClass(cls):
        cls.ref = ET.fromstring(zlib.decompress(FIXTURE.read_bytes()))
        # An instrument exercising all three control kinds: fader, note-toggle (button), label.
        inst = {"instrument": "fix", "nodes": [
            {"type": "clock", "address": "/clock", "control": {"label": "Tempo", "param": "tempo"}},
            {"type": "voicer", "address": "/voicer",
             "control": {"label": "Play", "widget": "note-toggle", "note": 60}},
        ]}
        controls = g.collect_controls(inst, META)
        cls.mine = ET.fromstring(zlib.decompress(g.build_tosc(inst, controls, 2)))

    def _keys(self, doc, ctype):
        n = doc.find(f".//node[@type='{ctype}']")
        return [p.find("key").text for p in n.findall("./properties/property")]

    def test_root_version_matches(self):
        self.assertEqual(self.mine.get("version"), self.ref.get("version"))

    def test_property_keys_match_reference(self):
        for ctype in ("FADER", "BUTTON", "LABEL"):
            self.assertEqual(self._keys(self.mine, ctype), self._keys(self.ref, ctype),
                             f"{ctype} property keys drifted from the reference export")


if __name__ == "__main__":
    unittest.main()

"""Tests for the control-surface generator (ADR-0018). Run: python3 -m unittest -v
(from this directory, or `python3 -m unittest discover .claude/skills/control-surface`).

These cover the deterministic half — metadata resolution, candidate inference, OSC addressing,
and the zlib/XML round-trip. Whether the emitted .tosc *loads in TouchOSC* is the on-device
verify step the skill calls out; it cannot be asserted here."""

import unittest
import zlib
from xml.etree import ElementTree as ET

import gen_surface as g

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


class EmitTest(unittest.TestCase):
    def setUp(self):
        controls = g.collect_controls(INSTRUMENT, META)
        self.doc = ET.fromstring(zlib.decompress(g.build_tosc(INSTRUMENT, controls, 4)))

    def test_round_trips_to_parseable_xml(self):
        self.assertEqual(self.doc.tag, "lexml")
        self.assertEqual(self.doc.get("version"), "3")

    def test_fader_addresses_and_scaling(self):
        # Find the /clock/tempo fader's OSC argument scaling — must map x[0,1] -> [1,999].
        partials = self.doc.findall(".//osc")
        found = {}
        for osc in partials:
            addr = osc.find("./path/partial").get("value")
            arg = osc.find("./arguments/partial")
            found[addr] = (arg.get("scaleMin"), arg.get("scaleMax"))
        self.assertEqual(found["/clock/tempo"], ("1.0", "999.0"))
        self.assertEqual(found["/brightness"], ("0.0", "100.0"))

    def test_send_only_one_way(self):
        osc = self.doc.find(".//osc")
        self.assertEqual(osc.find("send").text, "1")
        self.assertEqual(osc.find("receive").text, "0")

    def test_deterministic_bytes(self):
        controls = g.collect_controls(INSTRUMENT, META)
        a = g.build_tosc(INSTRUMENT, controls, 4)
        b = g.build_tosc(INSTRUMENT, controls, 4)
        self.assertEqual(a, b, "same instrument must emit identical bytes")


if __name__ == "__main__":
    unittest.main()

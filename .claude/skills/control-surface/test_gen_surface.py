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

# A tiny schema in the committed ADR-0028 shape: each input is a `oneOf` of a number form (the
# settable Float) and a wire-ref. One map (the Good Button workhorse) + a clock + a sequencer.
def _num(minimum, maximum, default, desc):
    return {"oneOf": [
        {"type": "number", "minimum": minimum, "maximum": maximum, "default": default, "description": desc},
        {"type": "object", "properties": {"from": {"type": "string"}}},
    ]}

SCHEMA = {
    "$defs": {"node": {"allOf": [
        {"if": {"properties": {"type": {"const": "map"}}},
         "then": {"properties": {"inputs": {"properties": {
             "in_min": _num(-1e6, 1e6, 0.0, "unit: , curve: Linear"),
             "in_max": _num(-1e6, 1e6, 1.0, "unit: , curve: Linear"),
             "default": _num(-1e6, 1e6, 0.0, "unit: , curve: Linear"),
         }}}}},
        {"if": {"properties": {"type": {"const": "clock"}}},
         "then": {"properties": {"inputs": {"properties": {
             "tempo": _num(1.0, 999.0, 120.0, "unit: BPM, curve: Linear"),
         }}}}},
        {"if": {"properties": {"type": {"const": "sequencer"}}},
         "then": {"properties": {"inputs": {"properties": {
             "step1": _num(0.0, 1.0, 0.0, "unit: , curve: Linear"),
             "step2": _num(0.0, 1.0, 0.0, "unit: , curve: Linear"),
         }}}}},
    ]}}
}

META = g.load_param_meta(SCHEMA)

# A Good Button map (public, no wired input), a ranged map fed by it (plumbing, an `inputs`
# wire-ref), and a clock with a tempo input.
INSTRUMENT = {
    "instrument": "t",
    "nodes": [
        {"type": "map", "address": "/brightness",
         "inputs": {"in_min": 0.0, "in_max": 100.0, "default": 50.0},
         "control": {"label": "Brightness", "unit": "%"}},
        {"type": "map", "address": "/map_cutoff",
         "inputs": {"in": {"from": "/brightness"}, "out_min": 400, "out_max": 12000}},
        {"type": "clock", "address": "/clock",
         "control": [{"label": "Tempo", "param": "tempo"}]},
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
    # No explicit `port`, so this exercises the default — which must be the voicer's real
    # `notes` input (routing matches the input port by name; `/voicer/note` would never land).
    NODE = {"type": "voicer", "address": "/voicer",
            "control": {"label": "Play C", "widget": "note-toggle", "note": 60}}

    def test_resolves_to_node_port_address_and_note(self):
        r = g.resolve_control(self.NODE, self.NODE["control"], META)
        self.assertEqual(r["kind"], "note-toggle")
        self.assertEqual(r["osc_addr"], "/voicer/notes")
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
        self.assertEqual(btn.find("./messages/osc/path/partial/value").text, "/voicer/notes")


class ParamToggleTest(unittest.TestCase):
    # A gate-mode sequencer lane: stepN are boolean on/off (ADR-0022), each carrying a control.
    LANE = {"type": "sequencer", "address": "/kick",
            "inputs": {"gate_mode": 1.0, "length": 16.0, "step1": 1.0, "step2": 0.0},
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
        node = dict(self.LANE, inputs={"gate_mode": 0.0})
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
            {"type": "sequencer", "address": "/kick", "inputs": {"gate_mode": 1.0}},
            {"label": f"K{i}", "param": f"step{i}"}, META) for i in range(1, 17)]
        controls.append(g.resolve_control(INSTRUMENT["nodes"][2], {"label": "Tempo", "param": "tempo"}, META))
        rows = g.layout_rows(controls, cols=4)
        self.assertEqual(len(rows[0]), 16, "all 16 lane steps share one row")
        self.assertTrue(all(c["kind"] == "param-toggle" for c in rows[0]))
        self.assertEqual([c["label"] for c in rows[-1]], ["Tempo"], "non-toggle controls grid below")


class GroupLayoutTest(unittest.TestCase):
    """A `group` hint packs consecutive controls onto one row regardless of `cols` — e.g. one
    drum channel per row, with an ungrouped control (tempo) flowing into the grid on its own."""

    def test_groups_become_their_own_rows(self):
        nodes = [
            {"type": "clock", "address": "/clock", "control": {"label": "Tempo", "param": "tempo"}},
            {"type": "clock", "address": "/k1", "control": {"label": "A", "param": "tempo", "group": "kick"}},
            {"type": "clock", "address": "/k2", "control": {"label": "B", "param": "tempo", "group": "kick"}},
            {"type": "clock", "address": "/k3", "control": {"label": "C", "param": "tempo", "group": "kick"}},
            {"type": "clock", "address": "/s1", "control": {"label": "D", "param": "tempo", "group": "snare"}},
            {"type": "clock", "address": "/s2", "control": {"label": "E", "param": "tempo", "group": "snare"}},
        ]
        controls = g.collect_controls({"instrument": "t", "nodes": nodes}, META)
        rows = g.layout_rows(controls, cols=4)
        labels = [[c["label"] for c in row] for row in rows]
        # Ungrouped tempo grids on its own row; each group is one full row, in declaration order.
        self.assertEqual(labels, [["Tempo"], ["A", "B", "C"], ["D", "E"]])


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


class RadialTest(unittest.TestCase):
    """A `radial` widget is a rotary fader: same value/OSC model, a RADIAL node instead of FADER."""

    def test_radial_widget_emits_radial_node_with_fader_scaling(self):
        node = INSTRUMENT["nodes"][2]  # the clock, with a tempo param
        c = g.resolve_control(node, {"label": "Tempo", "param": "tempo", "widget": "radial"}, META)
        self.assertEqual(c["kind"], "fader")          # still resolves through the fader path
        self.assertEqual(c["widget"], "radial")
        doc = ET.fromstring(zlib.decompress(g.build_tosc({"instrument": "t"}, [c], 1)))
        self.assertIsNone(doc.find(".//node[@type='FADER']"), "radial must not emit a FADER")
        rad = doc.find(".//node[@type='RADIAL']")
        self.assertIsNotNone(rad, "radial widget must emit a RADIAL node")
        # Same OSC send + range scaling as a fader: x[0,1] -> [1,999].
        arg = rad.find("./messages/osc/arguments/partial")
        self.assertEqual((arg.find("scaleMin").text, arg.find("scaleMax").text), ("1", "999"))

    def test_radial_frames_are_square(self):
        # A RADIAL draws a circle sized to its frame, so non-square frames overflow into
        # neighbouring cells. Every radial must get a square (w == h) frame, even in a wide row
        # or the lone control of a short last row (which a fader would stretch full-width).
        nodes = [{"type": "clock", "address": f"/c{i}",
                  "control": {"label": f"K{i}", "param": "tempo", "widget": "radial"}}
                 for i in range(5)]  # 5 in a 4-col grid -> a full row of 4 + a lone 5th
        controls = g.collect_controls({"instrument": "t", "nodes": nodes}, META)
        doc = ET.fromstring(zlib.decompress(g.build_tosc({"instrument": "t"}, controls, 4)))
        for rad in doc.findall(".//node[@type='RADIAL']"):
            f = [p for p in rad.findall("./properties/property") if p.find("key").text == "frame"][0]
            w, h = int(f.find("value/w").text), int(f.find("value/h").text)
            self.assertEqual(w, h, "radial frame must be square so the knob doesn't overflow")


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


class BoundaryTest(unittest.TestCase):
    """The nested-instrument path (ADR-0034 §4): one fader per wireable `interface` input, sourced
    from a `reuben describe --json` boundary view. Fed fixture boundary JSON directly (the merge is
    core's job, exercised by reuben's own tests) so these don't need the built binary."""

    # An instrument's `interface.inputs`: bare-target strings and an override object with `target`.
    IFACE = {
        "freq": "/osc.freq",
        "gate": "/env.gate",
        "tone": {"target": "/filter.cutoff", "label": "Tone", "min": 200, "max": 8000, "widget": "radial"},
        "in": "/filter.audio",     # driven inside the patch
        "mode": "/filter.mode",    # an enum — needs a selector widget, not a fader
    }
    # The matching `describe --json` boundary: kinds/ranges as core would report them (freq/tone
    # inherit + override, gate a held 0..1 `value` — issue #176 splits the numeric kinds into
    # `value` (held f32) vs `signal` (dense f32_buffer); both fader — `in` a driven bare audio
    # buffer, `mode` an enum).
    BOUNDARY = {"inputs": [
        {"name": "freq", "kind": "signal", "default": 440.0, "min": 20.0, "max": 20000.0, "unit": "Hz", "curve": "exponential"},
        {"name": "gate", "kind": "value", "default": 0.0, "min": 0.0, "max": 1.0, "curve": "linear"},
        {"name": "tone", "kind": "signal", "default": 4000.0, "min": 200.0, "max": 8000.0, "unit": "Hz", "curve": "exponential", "label": "Tone", "widget": "radial"},
        {"name": "in", "kind": "signal", "driven": True},
        {"name": "mode", "kind": "enum", "default": "lowpass", "variants": ["lowpass", "highpass"]},
    ]}

    def test_osc_address_is_the_interface_target(self):
        # `.` (the port separator) becomes `/`: the same reachable `/<node>/<input>` a direct
        # input uses (ADR-0034 §3).
        self.assertEqual(g._osc_from_target("/filter.cutoff"), "/filter/cutoff")

    def test_one_fader_per_wireable_input(self):
        controls = g.boundary_controls(self.IFACE, self.BOUNDARY)
        by_addr = {c["osc_addr"]: c for c in controls}
        # freq, gate, tone are wireable; `in` (driven) and `mode` (enum) are dropped.
        self.assertEqual(set(by_addr), {"/osc/freq", "/env/gate", "/filter/cutoff"})

    def test_inherited_metadata_flows_through(self):
        freq = next(c for c in g.boundary_controls(self.IFACE, self.BOUNDARY) if c["osc_addr"] == "/osc/freq")
        self.assertEqual((freq["min"], freq["max"], freq["default"], freq["unit"]), (20.0, 20000.0, 440.0, "Hz"))
        self.assertEqual(freq["label"], "Freq")   # no override -> titled from the boundary name
        self.assertEqual(freq["widget"], "fader")

    def test_overrides_win(self):
        tone = next(c for c in g.boundary_controls(self.IFACE, self.BOUNDARY) if c["osc_addr"] == "/filter/cutoff")
        self.assertEqual((tone["label"], tone["widget"]), ("Tone", "radial"))
        self.assertEqual((tone["min"], tone["max"]), (200.0, 8000.0))

    def test_driven_input_is_skipped(self):
        addrs = [c["osc_addr"] for c in g.boundary_controls(self.IFACE, self.BOUNDARY)]
        self.assertNotIn("/filter/audio", addrs, "a driven inner Signal port is not host-wireable")

    def test_non_fader_kinds_are_skipped(self):
        # An enum input needs a selector widget (out of scope); a bare audio signal has no range.
        audio_only = {"inputs": [{"name": "in", "kind": "signal"}]}  # no min/max -> not a fader
        self.assertEqual(g.boundary_controls({"in": "/x.audio"}, audio_only), [])

    def test_emits_faders_with_boundary_addresses_and_scaling(self):
        controls = g.boundary_controls(self.IFACE, self.BOUNDARY)
        doc = ET.fromstring(zlib.decompress(g.build_tosc({"instrument": "nested"}, controls, 4)))
        scaling = {}
        for osc in doc.findall(".//osc"):
            arg = osc.find("./arguments/partial")
            scaling[osc.find("./path/partial/value").text] = (arg.find("scaleMin").text, arg.find("scaleMax").text)
        self.assertEqual(scaling["/osc/freq"], ("20", "20000"))
        self.assertEqual(scaling["/filter/cutoff"], ("200", "8000"))
        # The radial override renders a RADIAL, not a FADER, for that control.
        self.assertIsNotNone(doc.find(".//node[@type='RADIAL']"))


class FixtureMatchTest(unittest.TestCase):
    """Lock the emitter to the known-good export: for each control type, our property *key set*
    must match the reference's. Catches format drift (a renamed/missing property) that unit
    tests on our own output alone would miss."""

    @classmethod
    def setUpClass(cls):
        cls.ref = ET.fromstring(zlib.decompress(FIXTURE.read_bytes()))
        # An instrument exercising every control kind: fader, radial, note-toggle (button), label.
        inst = {"instrument": "fix", "nodes": [
            {"type": "clock", "address": "/clock", "control": {"label": "Tempo", "param": "tempo"}},
            {"type": "clock", "address": "/clock2",
             "control": {"label": "Tempo2", "param": "tempo", "widget": "radial"}},
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
        for ctype in ("FADER", "RADIAL", "BUTTON", "LABEL"):
            self.assertEqual(self._keys(self.mine, ctype), self._keys(self.ref, ctype),
                             f"{ctype} property keys drifted from the reference export")


if __name__ == "__main__":
    unittest.main()

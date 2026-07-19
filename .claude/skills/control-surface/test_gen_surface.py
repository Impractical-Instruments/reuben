"""Tests for the control-surface generator (ADR-0043). Run: python3 -m unittest -v
(from this directory, or `python3 -m unittest discover .claude/skills/control-surface`).

These cover the deterministic half — surface-doc resolution against interface pipes, the
derived default surface, OSC addressing, layout, and the zlib/XML round-trip. Whether the
emitted .tosc *loads in TouchOSC* is the on-device verify step the skill calls out; it cannot
be asserted here."""

import json
import shutil
import subprocess
import tempfile
import unittest
import zlib
from pathlib import Path
from xml.etree import ElementTree as ET

import gen_surface as g

# A known-good TouchOSC export (built by hand in the editor) — the ground truth the emitter is
# cloned from. The structural-match test below fails if our output drifts from this format.
FIXTURE = Path(__file__).parent / "fixtures" / "REUBEN_REF.tosc"

# A compact instrument in the ADR-0043 shape: presentation-free interface input pipes carrying
# only the quantity contract (type/default/min/max/unit/curve, optional device `channel`).
# Covers every pipe classification the resolver distinguishes: ranged f32s, note (message)
# pipes, a ranged f32_buffer, a bare (rangeless) f32_buffer, and a channel-bound pipe.
INSTRUMENT = {
    "format_version": 3,
    "instrument": "t",
    "interface": {
        "inputs": {
            "brightness": {"type": "f32", "default": 50.0, "min": 0.0, "max": 100.0, "unit": "%"},
            "tempo": {"type": "f32", "default": 120.0, "min": 1.0, "max": 999.0, "unit": "BPM"},
            "notes": {"type": "note"},
            "chord": {"type": "note"},
            "tone": {"type": "f32_buffer", "default": 4000.0, "min": 20.0, "max": 20000.0,
                     "unit": "Hz", "curve": "exp"},
            "in": {"type": "f32_buffer"},
            "mic": {"type": "f32_buffer", "channel": 0},
            "kick_step1": {"type": "f32", "default": 1.0, "min": 0.0, "max": 1.0},
        },
        "outputs": {"out": {"from": "/mix.out"}},
    },
    "nodes": [],
}


def surface(*controls, **extra):
    """A minimal valid surface doc around the given controls."""
    doc = {"surface_version": 1, "instrument": "t", "controls": list(controls)}
    doc.update(extra)
    return doc


def resolve(*controls, instrument=INSTRUMENT, name="t", **extra):
    return g.resolve_surface(instrument, surface(*controls, **extra), name)


class DefaultLabelTest(unittest.TestCase):
    """The pinned default-label algorithm: "_" -> " ", then uppercase each word's first char."""

    def test_underscores_become_spaced_words(self):
        self.assertEqual(g.default_label("kick_step1"), "Kick Step1")
        self.assertEqual(g.default_label("kick_vol"), "Kick Vol")
        self.assertEqual(g.default_label("tone"), "Tone")

    def test_is_not_python_str_title(self):
        # str.title() also uppercases a letter FOLLOWING a digit ("mix2out" -> "Mix2Out");
        # the pinned algorithm (shared with the JS twin) only touches word-initial characters.
        self.assertEqual(g.default_label("mix2out"), "Mix2out")
        self.assertNotEqual(g.default_label("mix2out"), "mix2out".title())

    def test_rest_of_word_is_preserved(self):
        self.assertEqual(g.default_label("djFilter_LFO"), "DjFilter LFO")


class SurfaceResolveTest(unittest.TestCase):
    """Surface-doc resolution (ADR-0043): the pipe carries the quantity, the control the
    presentation; expected widgets are hand-written literals in the shared cross-impl shape."""

    def test_fader_resolves_to_exact_widget_literal(self):
        widgets, warnings = resolve({"bind": "tempo"})
        self.assertEqual(warnings, [])
        self.assertEqual(widgets, [{
            "kind": "fader", "widget": "fader", "bind": "tempo", "address": "/tempo/in",
            "label": "Tempo", "min": 1.0, "max": 999.0, "default": 120.0, "unit": "BPM",
        }])

    def test_int_pipe_infers_a_fader_and_detents_on_whole_values(self):
        # ADR-0061: an `i32` control pipe is a fader (not skipped), and carries a `steps` count so
        # the widget grid detents on each integer in [min, max].
        inst = {"format_version": 3, "instrument": "t", "interface": {"inputs": {
            "steps": {"type": "i32", "default": 16, "min": 1, "max": 16, "unit": "steps"}}},
            "nodes": []}
        widgets, warnings = resolve({"bind": "steps"}, instrument=inst)
        self.assertEqual(warnings, [])
        self.assertEqual(widgets, [{
            "kind": "fader", "widget": "fader", "bind": "steps", "address": "/steps/in",
            "label": "Steps", "min": 1, "max": 16, "default": 16, "unit": "steps", "steps": 15,
        }])

    def test_int_pipe_with_explicit_radial_widget_still_detents(self):
        inst = {"format_version": 3, "instrument": "t", "interface": {"inputs": {
            "rotation": {"type": "i32", "default": 0, "min": 0, "max": 15, "unit": "steps"}}},
            "nodes": []}
        widgets, _ = resolve({"bind": "rotation", "widget": "radial"}, instrument=inst)
        self.assertEqual((widgets[0]["kind"], widgets[0]["widget"], widgets[0]["steps"]),
                         ("fader", "radial", 15))

    def test_ranged_buffer_pipe_carries_unit_and_curve_from_the_pipe(self):
        widgets, warnings = resolve({"bind": "tone", "label": "Tone", "widget": "radial"})
        self.assertEqual(warnings, [])
        self.assertEqual(widgets, [{
            "kind": "fader", "widget": "radial", "bind": "tone", "address": "/tone/in",
            "label": "Tone", "min": 20.0, "max": 20000.0, "default": 4000.0,
            "unit": "Hz", "curve": "exp",
        }])

    def test_param_toggle_resolves_with_pipe_range_and_default(self):
        widgets, warnings = resolve(
            {"bind": "kick_step1", "label": "K1", "widget": "param-toggle", "group": "kick"})
        self.assertEqual(warnings, [])
        self.assertEqual(widgets, [{
            "kind": "param-toggle", "widget": "param-toggle", "bind": "kick_step1",
            "address": "/kick_step1/in", "label": "K1", "group": "kick",
            "min": 0.0, "max": 1.0, "default": 1.0,
        }])

    def test_note_toggle_payload_with_default_velocity(self):
        widgets, warnings = resolve({"bind": "notes", "widget": "note-toggle", "note": 60})
        self.assertEqual(warnings, [])
        self.assertEqual(widgets, [{
            "kind": "note-toggle", "widget": "note-toggle", "bind": "notes",
            "address": "/notes/in", "label": "Notes", "note": 60, "velocity": 1.0,
        }])

    def test_note_toggle_explicit_velocity_carries(self):
        widgets, _ = resolve({"bind": "notes", "widget": "note-toggle", "note": 62, "velocity": 0.5})
        self.assertEqual((widgets[0]["note"], widgets[0]["velocity"]), (62, 0.5))

    def test_chord_button_payload(self):
        widgets, warnings = resolve(
            {"bind": "chord", "widget": "chord-button", "degree": 3, "label": "IV"})
        self.assertEqual(warnings, [])
        self.assertEqual(widgets, [{
            "kind": "chord-button", "widget": "chord-button", "bind": "chord",
            "address": "/chord/in", "label": "IV", "degree": 3,
        }])

    def test_several_controls_may_bind_one_pipe(self):
        widgets, warnings = resolve(
            *[{"bind": "chord", "widget": "chord-button", "degree": d} for d in range(7)])
        self.assertEqual(warnings, [])
        self.assertEqual([w["degree"] for w in widgets], list(range(7)))
        self.assertEqual({w["address"] for w in widgets}, {"/chord/in"})

    def test_unknown_bind_warns_and_skips(self):
        widgets, warnings = resolve({"bind": "ghost"}, {"bind": "tempo"})
        self.assertEqual([w["bind"] for w in widgets], ["tempo"])
        self.assertEqual(len(warnings), 1)
        self.assertIn("surface control binds unknown pipe", warnings[0])
        self.assertIn("ghost", warnings[0])

    def test_reserved_and_unknown_widgets_skip_loudly(self):
        # The TouchOSC skip table (ADR-0043 §5): reserved/web-only/unrecognized kinds are
        # dropped with a warning naming each control, never silently.
        widgets, warnings = resolve(
            {"bind": "brightness", "widget": "xy-pad", "label": "Pad"},
            {"bind": "brightness", "widget": "wobble", "label": "Wob"},
            {"bind": "brightness", "label": "Brightness"})
        self.assertEqual([w["label"] for w in widgets], ["Brightness"])
        self.assertEqual(len(warnings), 2)
        self.assertIn("Pad", warnings[0])
        self.assertIn("xy-pad", warnings[0])
        self.assertIn("Wob", warnings[1])
        self.assertIn("wobble", warnings[1])

    def test_message_pipe_with_no_widget_warns_and_skips(self):
        widgets, warnings = resolve({"bind": "notes"})
        self.assertEqual(widgets, [])
        self.assertEqual(len(warnings), 1)
        self.assertIn("notes", warnings[0])

    def test_out_of_range_override_clamps_into_pipe_range_and_warns(self):
        widgets, warnings = resolve({"bind": "brightness", "min": -5.0, "max": 150.0})
        self.assertEqual((widgets[0]["min"], widgets[0]["max"]), (0.0, 100.0))
        self.assertEqual(len(warnings), 2, "one warning per out-of-range bound")
        self.assertTrue(all("Brightness" in w for w in warnings))

    def test_narrower_override_is_kept_without_warning(self):
        widgets, warnings = resolve({"bind": "brightness", "min": 10.0, "max": 90.0})
        self.assertEqual(warnings, [])
        self.assertEqual((widgets[0]["min"], widgets[0]["max"]), (10.0, 90.0))
        self.assertEqual(widgets[0]["default"], 50.0)

    def test_pipe_default_clamps_into_the_effective_range(self):
        # The pipe rests at 50; a narrowed [60, 90] presentation must rest at its own floor.
        widgets, _ = resolve({"bind": "brightness", "min": 60.0, "max": 90.0})
        self.assertEqual(widgets[0]["default"], 60.0)

    def test_pipe_without_default_rests_at_min(self):
        inst = {"instrument": "t2", "interface": {"inputs": {
            "raw": {"type": "f32", "min": 2.0, "max": 8.0}}}}
        widgets, _ = g.resolve_surface(inst, {
            "surface_version": 1, "instrument": "t2",
            "controls": [{"bind": "raw"}]}, "t2")
        self.assertEqual(widgets[0]["default"], 2.0)

    def test_default_label_is_applied_when_absent(self):
        widgets, _ = resolve({"bind": "kick_step1", "widget": "param-toggle"})
        self.assertEqual(widgets[0]["label"], "Kick Step1")

    def test_radial_is_kind_fader_widget_radial(self):
        widgets, _ = resolve({"bind": "tempo", "widget": "radial"})
        self.assertEqual((widgets[0]["kind"], widgets[0]["widget"]), ("fader", "radial"))

    def test_newer_surface_version_is_a_hard_error(self):
        doc = surface({"bind": "tempo"})
        doc["surface_version"] = 2
        with self.assertRaises(SystemExit):
            g.resolve_surface(INSTRUMENT, doc, "t")

    def test_instrument_name_mismatch_is_a_warning_only(self):
        doc = surface({"bind": "tempo"})
        doc["instrument"] = "other"
        widgets, warnings = g.resolve_surface(INSTRUMENT, doc, "t")
        self.assertEqual(len(widgets), 1, "a mismatch must not drop the surface")
        self.assertEqual(len(warnings), 1)
        self.assertIn("other", warnings[0])


class DefaultSurfaceTest(unittest.TestCase):
    """The derived default surface (ADR-0043 §3): one fader per wireable input pipe, declaration
    order; channel-bound, bare-audio, and message pipes are skipped with a warning naming each."""

    def test_one_fader_per_wireable_pipe_in_declaration_order(self):
        doc, _ = g.derive_surface(INSTRUMENT, "t")
        self.assertEqual(doc["surface_version"], 1)
        self.assertEqual(doc["instrument"], "t")
        self.assertEqual(doc["controls"], [
            {"bind": "brightness", "widget": "fader"},
            {"bind": "tempo", "widget": "fader"},
            {"bind": "tone", "widget": "fader"},
            {"bind": "kick_step1", "widget": "fader"},
        ])

    def test_unwireable_pipes_are_skipped_with_a_warning_each(self):
        _, warnings = g.derive_surface(INSTRUMENT, "t")
        named = [w for pipe in ("notes", "chord", "in", "mic") for w in warnings if f"'{pipe}'" in w]
        self.assertEqual(len(warnings), 4)
        self.assertEqual(len(named), 4, f"each skipped pipe must be named: {warnings}")

    def test_derived_surface_resolves_end_to_end(self):
        doc, _ = g.derive_surface(INSTRUMENT, "t")
        widgets, warnings = g.resolve_surface(INSTRUMENT, doc, "t")
        self.assertEqual(warnings, [])
        self.assertEqual([w["address"] for w in widgets],
                         ["/brightness/in", "/tempo/in", "/tone/in", "/kick_step1/in"])
        self.assertTrue(all(w["kind"] == "fader" and w["widget"] == "fader" for w in widgets))


class SurfaceDocLookupTest(unittest.TestCase):
    """File resolution order (ADR-0043 §5): <stem>.<target>.json ?? <stem>.json ?? None."""

    def test_lookup_order(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            self.assertIsNone(g.find_surface_doc(root, "t"))
            generic = root / "t.json"
            generic.write_text("{}")
            self.assertEqual(g.find_surface_doc(root, "t"), generic)
            per_target = root / "t.touchosc.json"
            per_target.write_text("{}")
            self.assertEqual(g.find_surface_doc(root, "t"), per_target)


class NoteToggleTest(unittest.TestCase):
    def widget(self):
        widgets, _ = resolve({"bind": "notes", "label": "Play C", "widget": "note-toggle", "note": 60})
        return widgets[0]

    def test_resolves_to_pipe_in_address_and_note(self):
        c = self.widget()
        self.assertEqual(c["kind"], "note-toggle")
        self.assertEqual(c["address"], "/notes/in")
        self.assertEqual(c["note"], 60)

    def test_emits_a_toggle_button_with_constant_note_and_gate(self):
        doc = ET.fromstring(zlib.decompress(g.build_tosc("t", [self.widget()], 1)))
        btn = doc.find(".//node[@type='BUTTON']")
        self.assertIsNotNone(btn, "note-toggle must emit a BUTTON")
        btype = [p for p in btn.findall("./properties/property") if p.find("key").text == "buttonType"][0]
        self.assertEqual(btype.find("value").text, "2", "buttonType 2 = Toggle Press")
        args = btn.findall("./messages/osc/arguments/partial")
        self.assertEqual(args[0].find("type").text, "CONSTANT")
        self.assertEqual(args[0].find("value").text, "60")   # the fixed MIDI note
        self.assertEqual(args[1].find("type").text, "VALUE")
        self.assertEqual(args[1].find("value").text, "x")    # the gate
        self.assertEqual(btn.find("./messages/osc/path/partial/value").text, "/notes/in")


# A groovebox-style instrument for the lane tests: 16 gate-step pipes + a tempo pipe.
LANE_INSTRUMENT = {
    "instrument": "gb",
    "interface": {"inputs": dict(
        [(f"kick_step{i}", {"type": "f32", "default": 1.0 if i in (1, 5) else 0.0,
                            "min": 0.0, "max": 1.0}) for i in range(1, 17)]
        + [("tempo", {"type": "f32", "default": 120.0, "min": 1.0, "max": 999.0, "unit": "BPM"})],
    )},
}


def lane_surface(steps=16, group="kick", trailing_tempo=True):
    controls = [{"bind": f"kick_step{i}", "label": f"K{i}", "widget": "param-toggle", "group": group}
                for i in range(1, steps + 1)]
    if trailing_tempo:
        controls.append({"bind": "tempo", "label": "Tempo"})
    return {"surface_version": 1, "instrument": "gb", "controls": controls}


class ParamToggleTest(unittest.TestCase):
    def test_default_mirrors_the_pipe_resting_value(self):
        widgets, _ = g.resolve_surface(LANE_INSTRUMENT, lane_surface(2, trailing_tempo=False), "gb")
        self.assertEqual([w["default"] for w in widgets], [1.0, 0.0])

    def test_emits_a_button_sending_x_to_the_pipe(self):
        widgets, _ = g.resolve_surface(LANE_INSTRUMENT, lane_surface(1, trailing_tempo=False), "gb")
        doc = ET.fromstring(zlib.decompress(g.build_tosc("gb", widgets, 4)))
        btn = doc.find(".//node[@type='BUTTON']")
        self.assertIsNotNone(btn, "param-toggle must emit a BUTTON")
        btype = [p for p in btn.findall("./properties/property") if p.find("key").text == "buttonType"][0]
        self.assertEqual(btype.find("value").text, "2", "buttonType 2 = Toggle Press")
        self.assertEqual(btn.find("./messages/osc/path/partial/value").text, "/kick_step1/in")
        args = btn.findall("./messages/osc/arguments/partial")
        self.assertEqual(len(args), 1, "the button's x IS the payload — no constant note")
        self.assertEqual(args[0].find("type").text, "VALUE")
        self.assertEqual(args[0].find("value").text, "x")

    def test_lane_packs_into_one_full_width_row(self):
        # 16 steps + a trailing fader: the steps share one row of 16; the fader drops below.
        widgets, _ = g.resolve_surface(LANE_INSTRUMENT, lane_surface(16), "gb")
        rows = g.layout_rows(widgets, cols=4)
        self.assertEqual(len(rows[0]), 16, "all 16 lane steps share one row")
        self.assertTrue(all(c["kind"] == "param-toggle" for c in rows[0]))
        self.assertEqual([c["label"] for c in rows[-1]], ["Tempo"], "non-toggle controls grid below")

    def test_adjacent_lanes_with_different_groups_get_their_own_rows(self):
        # Steps are ordinary pipes now (ADR-0043 §6), so a lane is identified by its shared
        # `group` hint — two back-to-back lanes must not merge into one run.
        inst = {"instrument": "gb2", "interface": {"inputs": {
            **{f"kick_step{i}": {"type": "f32", "default": 0.0, "min": 0.0, "max": 1.0} for i in (1, 2, 3)},
            **{f"snare_step{i}": {"type": "f32", "default": 0.0, "min": 0.0, "max": 1.0} for i in (1, 2)},
        }}}
        controls = ([{"bind": f"kick_step{i}", "widget": "param-toggle", "group": "kick"} for i in (1, 2, 3)]
                    + [{"bind": f"snare_step{i}", "widget": "param-toggle", "group": "snare"} for i in (1, 2)])
        widgets, _ = g.resolve_surface(
            inst, {"surface_version": 1, "instrument": "gb2", "controls": controls}, "gb2")
        rows = g.layout_rows(widgets, cols=4)
        self.assertEqual([[c["bind"] for c in row] for row in rows],
                         [["kick_step1", "kick_step2", "kick_step3"],
                          ["snare_step1", "snare_step2"]])


class GroupLayoutTest(unittest.TestCase):
    """A `group` hint packs consecutive controls onto one row regardless of `cols` — e.g. one
    drum channel per row, with an ungrouped control (tempo) flowing into the grid on its own."""

    def test_groups_become_their_own_rows(self):
        controls = [
            {"bind": "tempo", "label": "Tempo"},
            {"bind": "brightness", "label": "A", "group": "kick"},
            {"bind": "brightness", "label": "B", "group": "kick"},
            {"bind": "brightness", "label": "C", "group": "kick"},
            {"bind": "tone", "label": "D", "group": "snare"},
            {"bind": "tone", "label": "E", "group": "snare"},
        ]
        widgets, warnings = resolve(*controls)
        self.assertEqual(warnings, [])
        rows = g.layout_rows(widgets, cols=4)
        labels = [[c["label"] for c in row] for row in rows]
        # Ungrouped tempo grids on its own row; each group is one full row, in declaration order.
        self.assertEqual(labels, [["Tempo"], ["A", "B", "C"], ["D", "E"]])


class ChordButtonTest(unittest.TestCase):
    """Chord buttons (ADR-0043 §1): a toggle whose payload is `[degree, gate]`, sent to the
    bound note pipe's `/in` port (the OSC boundary converts the degree payload at a note port).
    Same 2-arg button mechanism as note-toggle, but the constant is a scale degree."""

    def widget(self):
        widgets, _ = resolve({"bind": "chord", "label": "IV", "widget": "chord-button", "degree": 3})
        return widgets[0]

    def test_resolves_to_pipe_in_address_and_degree(self):
        c = self.widget()
        self.assertEqual(c["kind"], "chord-button")
        self.assertEqual(c["address"], "/chord/in")
        self.assertEqual(c["degree"], 3)

    def test_emits_a_toggle_button_with_constant_degree_and_gate(self):
        doc = ET.fromstring(zlib.decompress(g.build_tosc("t", [self.widget()], 1)))
        btn = doc.find(".//node[@type='BUTTON']")
        self.assertIsNotNone(btn, "chord-button must emit a BUTTON")
        btype = [p for p in btn.findall("./properties/property") if p.find("key").text == "buttonType"][0]
        self.assertEqual(btype.find("value").text, "2", "buttonType 2 = Toggle Press")
        args = btn.findall("./messages/osc/arguments/partial")
        self.assertEqual(args[0].find("type").text, "CONSTANT")
        self.assertEqual(args[0].find("value").text, "3")    # the fixed chord-root degree
        self.assertEqual(args[1].find("type").text, "VALUE")
        self.assertEqual(args[1].find("value").text, "x")    # the gate
        self.assertEqual(btn.find("./messages/osc/path/partial/value").text, "/chord/in")

    def test_seven_chord_buttons_each_send_their_own_degree(self):
        # The Chord-player surface: 7 buttons, root degrees 0..6, all on one note pipe.
        widgets, _ = resolve(
            *[{"bind": "chord", "label": f"d{d}", "widget": "chord-button", "degree": d}
              for d in range(7)])
        self.assertEqual(len(widgets), 7)
        doc = ET.fromstring(zlib.decompress(g.build_tosc("cp", widgets, 7)))
        degrees = []
        for btn in doc.findall(".//node[@type='BUTTON']"):
            addr = btn.find("./messages/osc/path/partial/value").text
            self.assertEqual(addr, "/chord/in", "all chord buttons hit one pipe address")
            degrees.append(btn.findall("./messages/osc/arguments/partial")[0].find("value").text)
        self.assertEqual(degrees, ["0", "1", "2", "3", "4", "5", "6"])


class RadialTest(unittest.TestCase):
    """A `radial` widget is a rotary fader: same value/OSC model, a RADIAL node instead of FADER."""

    def test_radial_widget_emits_radial_node_with_fader_scaling(self):
        widgets, _ = resolve({"bind": "tempo", "label": "Tempo", "widget": "radial"})
        c = widgets[0]
        self.assertEqual(c["kind"], "fader")          # still resolves through the fader path
        self.assertEqual(c["widget"], "radial")
        doc = ET.fromstring(zlib.decompress(g.build_tosc("t", [c], 1)))
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
        widgets, _ = resolve(
            *[{"bind": "tempo", "label": f"K{i}", "widget": "radial"} for i in range(5)])
        # 5 in a 4-col grid -> a full row of 4 + a lone 5th
        doc = ET.fromstring(zlib.decompress(g.build_tosc("t", widgets, 4)))
        for rad in doc.findall(".//node[@type='RADIAL']"):
            f = [p for p in rad.findall("./properties/property") if p.find("key").text == "frame"][0]
            w, h = int(f.find("value/w").text), int(f.find("value/h").text)
            self.assertEqual(w, h, "radial frame must be square so the knob doesn't overflow")


class EmitTest(unittest.TestCase):
    def setUp(self):
        self.widgets, _ = resolve({"bind": "brightness", "label": "Brightness"},
                                  {"bind": "tempo", "label": "Tempo"})
        self.doc = ET.fromstring(zlib.decompress(g.build_tosc("t", self.widgets, 4)))

    def test_round_trips_to_parseable_xml(self):
        # Matches the editor's export: lexml version 6.
        self.assertEqual(self.doc.tag, "lexml")
        self.assertEqual(self.doc.get("version"), "6")

    def test_fader_addresses_and_scaling(self):
        # Partials use child elements (not attributes). The argument scaling maps x[0,1] to the
        # pipe's real range, at the pipe's own `/in` port (ADR-0043 §1).
        found = {}
        for osc in self.doc.findall(".//osc"):
            addr = osc.find("./path/partial/value").text
            arg = osc.find("./arguments/partial")
            found[addr] = (arg.find("scaleMin").text, arg.find("scaleMax").text)
        self.assertEqual(found["/tempo/in"], ("1", "999"))
        self.assertEqual(found["/brightness/in"], ("0", "100"))

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
        a = g.build_tosc("t", self.widgets, 4)
        b = g.build_tosc("t", self.widgets, 4)
        self.assertEqual(a, b, "same instrument must emit identical bytes")


class EmitCliTest(unittest.TestCase):
    """End-to-end `emit` through main(): explicit --surface, and the derived-default fallback."""

    def test_emit_with_explicit_surface_doc(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            inst = root / "t.json"
            inst.write_text(json.dumps(INSTRUMENT))
            surf = root / "t.surface.json"
            surf.write_text(json.dumps(surface({"bind": "tempo", "label": "Tempo"})))
            out = root / "t.tosc"
            rc = g.main(["emit", str(inst), "--surface", str(surf), "--out", str(out)])
            self.assertEqual(rc, 0)
            doc = ET.fromstring(zlib.decompress(out.read_bytes()))
            addrs = [o.find("./path/partial/value").text for o in doc.findall(".//osc")]
            self.assertEqual(addrs, ["/tempo/in"])

    def test_emit_derives_the_default_surface_when_no_doc_exists(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            # A stem no committed surfaces/*.json matches, so the lookup falls through to derive.
            inst = root / "zz-derived-default.json"
            inst.write_text(json.dumps(INSTRUMENT))
            out = root / "zz.tosc"
            rc = g.main(["emit", str(inst), "--out", str(out)])
            self.assertEqual(rc, 0)
            doc = ET.fromstring(zlib.decompress(out.read_bytes()))
            addrs = [o.find("./path/partial/value").text for o in doc.findall(".//osc")]
            self.assertEqual(addrs, ["/brightness/in", "/tempo/in", "/tone/in", "/kick_step1/in"])


class CommittedSurfaceDocTest(unittest.TestCase):
    """The behavioral guards that replaced the retired expected-widgets oracle. For every
    committed `surfaces/*.json`: (1) it satisfies `surfaces/surface.schema.json`, and (2) it
    resolves against its instrument with no warnings and at least one widget. These assert
    *properties* — well-formed, binds land, clean resolve — not a byte-for-byte snapshot, so a
    legitimate range/label/reorder edit passes; only a genuinely broken doc fails.

    The validator is schema-driven: the allowed keys, the `widget` enum, and the
    `widget -> required payload` conditionals are read out of the schema file, so editing the
    schema keeps the check honest without hand-syncing a second copy of its rules here.
    """

    REPO_ROOT = Path(__file__).resolve().parents[3]
    SURFACES = REPO_ROOT / "surfaces"
    SCHEMA = json.loads((SURFACES / "surface.schema.json").read_text())
    DOCS = sorted(p for p in SURFACES.glob("*.json") if p.name != "surface.schema.json")

    def _schema_errors(self, doc):
        """Violations of surface.schema.json (empty list == valid). A hand-rolled check of the
        JSON-Schema keyword subset this schema uses — the runner is bare `python3 -m unittest`,
        so there is no `jsonschema` dependency to lean on."""
        s, ctrl = self.SCHEMA, self.SCHEMA["$defs"]["control"]
        errs = []
        if not isinstance(doc, dict):
            return ["document is not a JSON object"]
        top_keys = set(s["properties"])
        errs += [f"missing required key {k!r}" for k in s["required"] if k not in doc]
        errs += [f"unknown top-level key {k!r}" for k in doc if k not in top_keys]
        const_v = s["properties"]["surface_version"]["const"]
        if doc.get("surface_version") != const_v:
            errs.append(f"surface_version {doc.get('surface_version')!r} must be {const_v}")
        if "instrument" in doc and not (isinstance(doc["instrument"], str) and doc["instrument"]):
            errs.append("instrument must be a non-empty string")
        if "cols" in doc and not (isinstance(doc["cols"], int) and doc["cols"] >= 1):
            errs.append("cols must be an integer >= 1")
        controls = doc.get("controls")
        if not isinstance(controls, list):
            return errs + ["controls must be an array"]
        ctrl_keys = set(ctrl["properties"])
        widgets = ctrl["properties"]["widget"]["enum"]
        for i, c in enumerate(controls):
            at = f"controls[{i}]"
            if not isinstance(c, dict):
                errs.append(f"{at} is not an object")
                continue
            errs += [f"{at} missing required key {k!r}" for k in ctrl["required"] if k not in c]
            errs += [f"{at} unknown key {k!r}" for k in c if k not in ctrl_keys]
            if "bind" in c and not (isinstance(c["bind"], str) and c["bind"]):
                errs.append(f"{at} bind must be a non-empty string")
            for strk in ("label", "group"):
                if strk in c and not isinstance(c[strk], str):
                    errs.append(f"{at} {strk} must be a string")
            if "widget" in c and c["widget"] not in widgets:
                errs.append(f"{at} widget {c['widget']!r} not one of {widgets}")
            for numk in ("min", "max", "velocity"):
                if numk in c and not isinstance(c[numk], (int, float)):
                    errs.append(f"{at} {numk} must be a number")
            if isinstance(c.get("velocity"), (int, float)) and not 0 <= c["velocity"] <= 1:
                errs.append(f"{at} velocity {c['velocity']} out of [0, 1]")
            for intk in ("note", "degree"):
                if intk in c and not isinstance(c[intk], int):
                    errs.append(f"{at} {intk} must be an integer")
            # Schema-driven conditionals: a widget const implies a required payload key.
            for rule in ctrl.get("allOf", []):
                want = rule.get("if", {}).get("properties", {}).get("widget", {}).get("const")
                if want is not None and c.get("widget") == want:
                    errs += [f"{at} widget {want!r} requires {k!r}"
                             for k in rule.get("then", {}).get("required", []) if k not in c]
        return errs

    def test_committed_surfaces_satisfy_schema(self):
        self.assertTrue(self.DOCS, "no committed surface docs found under surfaces/")
        for p in self.DOCS:
            with self.subTest(surface=p.name):
                self.assertEqual(self._schema_errors(json.loads(p.read_text())), [],
                                 f"{p.name} violates surface.schema.json")

    def test_validator_has_teeth(self):
        """A green schema test is worthless if the validator accepts everything. Pin that each
        rule actually fires — so this suite can't rot into the very thing it replaced."""
        base = json.loads((self.SURFACES / f"{self.DOCS[0].stem}.json").read_text()) \
            if self.DOCS else {"surface_version": 1, "instrument": "x", "controls": []}
        cases = [
            ({**base, "bogus": 1}, "unknown top-level key"),
            ({k: v for k, v in base.items() if k != "controls"}, "missing required key"),
            ({**base, "surface_version": 2}, "surface_version"),
            ({**base, "controls": [{"label": "no bind"}]}, "missing required key 'bind'"),
            ({**base, "controls": [{"bind": "x", "typo": 1}]}, "unknown key 'typo'"),
            ({**base, "controls": [{"bind": "x", "widget": "slider"}]}, "not one of"),
            ({**base, "controls": [{"bind": "x", "widget": "note-toggle"}]}, "requires 'note'"),
            ({**base, "controls": [{"bind": "x", "widget": "chord-button"}]}, "requires 'degree'"),
        ]
        for doc, needle in cases:
            with self.subTest(expect=needle):
                errs = self._schema_errors(doc)
                self.assertTrue(any(needle in e for e in errs),
                                f"expected an error containing {needle!r}, got {errs}")

    def test_committed_surfaces_resolve_clean(self):
        for p in self.DOCS:
            with self.subTest(surface=p.name):
                doc = json.loads(p.read_text())
                found = list((self.REPO_ROOT / "instruments").rglob(f"{doc['instrument']}.json"))
                self.assertEqual(len(found), 1,
                                 f"expected one instrument named {doc['instrument']!r}, got {found}")
                widgets, warnings = g.resolve_surface(json.loads(found[0].read_text()), doc, p.stem)
                self.assertEqual(warnings, [], f"{p.name} must resolve without warnings")
                self.assertTrue(widgets, f"{p.name} produced no widgets")


class BoundaryTest(unittest.TestCase):
    """The interface-pipe path (ADR-0038 §2): one fader per wireable `interface` input pipe, sourced
    from a `reuben describe --json` boundary view. Fed fixture boundary JSON directly (the view is
    core's job, exercised by reuben's own tests) so these don't need the built binary. In v2 an
    input pipe declares its own type + owned metadata and mints its own address — there is no inner
    target to read from the document, so `boundary_controls` takes the describe view alone and the
    OSC address is the pipe's `/<name>/in` port."""

    # The `describe --json` boundary: kinds/ranges as core would report them for space.json-style
    # input pipes. issue #176 splits the numeric kinds into `value` (held f32) vs `signal` (dense
    # f32_buffer); both fader. `in` is a bare audio pipe (no range); `mode` an enum.
    BOUNDARY = {"inputs": [
        {"name": "freq", "kind": "signal", "default": 440.0, "min": 20.0, "max": 20000.0, "unit": "Hz", "curve": "exponential"},
        {"name": "gate", "kind": "value", "default": 0.0, "min": 0.0, "max": 1.0, "curve": "linear"},
        # `label`/`widget` here are STALE keys a pre-ADR-0043 describe could emit — the
        # boundary path must ignore them (presentation lives in a surface doc; describe no
        # longer carries any). Kept in the fixture to guard against re-reading them.
        {"name": "tone", "kind": "signal", "default": 4000.0, "min": 200.0, "max": 8000.0, "unit": "Hz", "curve": "exponential", "label": "Legacy", "widget": "radial"},
        {"name": "in", "kind": "signal"},
        {"name": "mode", "kind": "enum", "default": "lowpass", "variants": ["lowpass", "highpass"]},
    ]}

    def test_osc_address_is_the_pipe_in_port(self):
        # ADR-0038: a pipe mints its own address and takes control on its `in` port -> `/<name>/in`.
        self.assertEqual(g._osc_from_pipe("tone"), "/tone/in")

    def test_one_fader_per_wireable_input(self):
        controls = g.boundary_controls(self.BOUNDARY)
        by_addr = {c["address"]: c for c in controls}
        # freq, gate, tone are wireable; `in` (bare audio, no range) and `mode` (enum) are dropped.
        self.assertEqual(set(by_addr), {"/freq/in", "/gate/in", "/tone/in"})

    def test_declared_metadata_flows_through(self):
        freq = next(c for c in g.boundary_controls(self.BOUNDARY) if c["address"] == "/freq/in")
        self.assertEqual((freq["min"], freq["max"], freq["default"], freq["unit"]), (20.0, 20000.0, 440.0, "Hz"))
        self.assertEqual(freq["label"], "Freq")   # the one pinned default_label algorithm
        self.assertEqual(freq["widget"], "fader")

    def test_stale_describe_presentation_is_ignored(self):
        # ADR-0043: describe carries no presentation; a stale `label`/`widget` key in a
        # pre-captured describe view must not leak through (labels come from default_label,
        # every boundary control is a plain fader).
        tone = next(c for c in g.boundary_controls(self.BOUNDARY) if c["address"] == "/tone/in")
        self.assertEqual((tone["label"], tone["widget"]), ("Tone", "fader"))
        self.assertEqual((tone["min"], tone["max"]), (200.0, 8000.0))

    def test_non_fader_kinds_are_skipped(self):
        # An enum input needs a selector widget (out of scope); a bare audio pipe has no range.
        audio_only = {"inputs": [{"name": "in", "kind": "signal"}]}  # no min/max -> not a fader
        self.assertEqual(g.boundary_controls(audio_only), [])

    def test_emits_faders_with_boundary_addresses_and_scaling(self):
        controls = g.boundary_controls(self.BOUNDARY)
        doc = ET.fromstring(zlib.decompress(g.build_tosc("nested", controls, 4)))
        scaling = {}
        for osc in doc.findall(".//osc"):
            arg = osc.find("./arguments/partial")
            scaling[osc.find("./path/partial/value").text] = (arg.find("scaleMin").text, arg.find("scaleMax").text)
        self.assertEqual(scaling["/freq/in"], ("20", "20000"))
        self.assertEqual(scaling["/tone/in"], ("200", "8000"))
        # Every boundary control is a plain fader — radial (and every other widget kind)
        # is surface-doc curation now, never a describe-carried hint.
        self.assertIsNone(doc.find(".//node[@type='RADIAL']"))


class LiveEngineBoundaryTest(unittest.TestCase):
    """Bind the boundary path to the **live engine**, not a hand-written fixture. `BoundaryTest`
    above feeds `boundary_controls` a describe view *we* wrote, so it stays green even if the real
    `describe --json` shape flips under it — exactly the blind spot that let issue #233 rot (the
    interface `target`→pipe direction flip, ADR-0038, silently retired the v1 shape this script
    hand-decoded). This test runs the real `reuben describe` on the committed v3 instrument
    `instruments/patches/space.json` and asserts the surface, so a future breaking format change
    breaks *here* instead of drifting silently.

    It shells out via `cargo run` (the same command the skills document) rather than a pre-built
    binary, so it always reflects *current source* — a stale `target/` binary can't make it pass
    or fail spuriously. It needs the Rust toolchain; when `cargo` is absent it skips loudly rather
    than passing vacuously. (Precedent: `FixtureMatchTest` does the same for the .tosc format
    against a real editor export. This is that discipline for the *engine contract* the boundary
    path consumes.)

    KNOWN LIMITATION: the `/<name>/in` OSC address scheme is *synthesized by the skill*
    (`_osc_from_pipe`) and never appears in `describe --json`, which emits only the bare pipe
    `name`. So this test guards the pipe *surface* the engine reports — names, ranges, kinds — not
    the address scheme layered on top: a port-rename or address-shape drift (e.g. the pipe's control
    port going `in`→`set`) would slip past it, because the address is our invention, not the
    engine's. `test_osc_address_is_the_pipe_in_port` pins the scheme itself."""

    REPO_ROOT = Path(__file__).resolve().parents[3]
    SPACE = REPO_ROOT / "instruments" / "patches" / "space.json"
    # `cargo run`/`cargo build` guard: a target-dir lock (a parallel cargo holding it) otherwise
    # hangs the suite silently. Generous enough for a cold first compile.
    CARGO_TIMEOUT = 300

    def _live_describe(self):
        if shutil.which("cargo") is None:
            self.skipTest("cargo not on PATH — the live-engine boundary test needs the Rust "
                          "toolchain to build a current `reuben` (guards ADR-0038 pipe drift)")
        # `--features reuben-core/bench` mirrors CI's clippy/test steps so this shares their
        # compiled artifacts instead of forcing a featureless rebuild (the `bench` feature is inert
        # for `describe`).
        cmd = ["cargo", "run", "-q", "-p", "reuben-native", "--bin", "reuben",
               "--features", "reuben-core/bench", "--",
               "describe", str(self.SPACE), "--json"]
        try:
            proc = subprocess.run(cmd, cwd=self.REPO_ROOT, capture_output=True, text=True,
                                  timeout=self.CARGO_TIMEOUT)
        except subprocess.TimeoutExpired as e:
            stderr = e.stderr.decode() if isinstance(e.stderr, bytes) else (e.stderr or "")
            self.fail(f"`cargo run ... describe` timed out after {self.CARGO_TIMEOUT}s "
                      f"(a held target-dir lock?):\n{stderr}")
        self.assertEqual(proc.returncode, 0, f"`reuben describe` failed:\n{proc.stderr}")
        return json.loads(proc.stdout)

    def test_live_describe_surfaces_v2_pipe_addresses(self):
        by_addr = {c["address"]: c for c in g.boundary_controls(self._live_describe())}
        # The two regressions #233 fixed, asserted against *real* engine output:
        #  - non-empty (v1-decoding produced zero controls on a v2 doc), and
        #  - addressed by the pipe's own `/<name>/in` port (ADR-0038), not a v1 inner target.
        # space.json's ranged pipes `space` (0..1 value) + `tone` (20..20000 signal) surface;
        # the bare-audio `in` pipe (no range) is skipped.
        self.assertEqual(set(by_addr), {"/space/in", "/tone/in"})
        # Derive the expected range/unit from the source doc's declared `tone` pipe rather than
        # hardcoding the musical literals, so a legitimate retune of space.json doesn't read as a
        # describe-format regression. What's under test is that the engine round-trips the pipe's
        # *declared* metadata out through `describe`.
        tone_pipe = json.loads(self.SPACE.read_text())["interface"]["inputs"]["tone"]
        tone = by_addr["/tone/in"]
        self.assertEqual((tone["min"], tone["max"], tone["unit"]),
                         (tone_pipe["min"], tone_pipe["max"], tone_pipe["unit"]))

    def test_run_describe_and_find_reuben_cover_the_prebuilt_path(self):
        """Exercise the production `find_reuben` + `run_describe` helpers, which `_live_describe`
        deliberately bypasses (it wants `cargo run` to reflect current source). Builds the binary
        once — reusing `_live_describe`'s artifacts, same feature set — then drives the helpers
        against it, so their subprocess/returncode/JSON handling isn't left with zero live coverage."""
        if shutil.which("cargo") is None:
            self.skipTest("cargo not on PATH — needs the Rust toolchain to build `reuben`")
        build = subprocess.run(
            ["cargo", "build", "-q", "-p", "reuben-native", "--bin", "reuben",
             "--features", "reuben-core/bench"],
            cwd=self.REPO_ROOT, capture_output=True, text=True, timeout=self.CARGO_TIMEOUT,
        )
        self.assertEqual(build.returncode, 0, f"cargo build failed:\n{build.stderr}")
        reuben_bin = g.find_reuben(Path(g.__file__), None)
        self.assertNotEqual(reuben_bin, "reuben", "find_reuben should locate the freshly built binary")
        by_addr = {c["address"] for c in g.boundary_controls(g.run_describe(reuben_bin, self.SPACE))}
        self.assertEqual(by_addr, {"/space/in", "/tone/in"})


class FixtureMatchTest(unittest.TestCase):
    """Lock the emitter to the known-good export: for each control type, our property *key set*
    must match the reference's. Catches format drift (a renamed/missing property) that unit
    tests on our own output alone would miss."""

    @classmethod
    def setUpClass(cls):
        cls.ref = ET.fromstring(zlib.decompress(FIXTURE.read_bytes()))
        # A surface exercising every control kind: fader, radial, note-toggle (button), label.
        widgets, _ = resolve(
            {"bind": "tempo", "label": "Tempo"},
            {"bind": "tone", "label": "Tone", "widget": "radial"},
            {"bind": "notes", "label": "Play", "widget": "note-toggle", "note": 60})
        cls.mine = ET.fromstring(zlib.decompress(g.build_tosc("fix", widgets, 2)))

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

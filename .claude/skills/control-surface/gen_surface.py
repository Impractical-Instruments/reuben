#!/usr/bin/env python3
"""Generate a Hexler TouchOSC control surface (.tosc) from a reuben instrument (ADR-0018).

The deterministic half of the `control-surface` skill: the agent decides *which* controls a
surface should have and labels them (writing `control` blocks into the instrument JSON); this
script does the mechanical, repeatable parts — resolving each control's OSC address + value
range from authoritative metadata, and emitting the gzip/zlib-packed XML TouchOSC reads.

Two subcommands:

  infer  INSTRUMENT [--schema P]
      Read-only. Print JSON describing every control *candidate* — externally-driven Good
      Buttons (a `map` whose message input has no incoming connection) and each node param,
      with its resolved address, range, unit and default. The agent curates this into
      `control` blocks. Nothing is written.

  emit   INSTRUMENT [--schema P] [--host H] [--port N] [--out F] [--cols N]
      Read-only on the instrument. Resolve every node that carries a `control` block and write
      a `.tosc` surface targeting `host:port`.

Metadata is read from the committed instrument schema (the per-type param ranges + unit/curve),
which is kept in sync with the operator descriptors by the `schema_is_in_sync` test — so this
script never re-implements operator metadata, it reads the single source of truth.

OSC addressing (verified against the core router, ADR-0011):
  - Good Button (a `map` front-end): the widget sends to the node address, e.g. `/brightness`.
    Range = the node's `in_min`/`in_max` *instance* param values (default 0..1).
  - Direct param:                    the widget sends to `/<node>/<param>`, e.g. `/clock/tempo`.
    Range = the param's schema min/max; unit/default likewise.
"""

import argparse
import json
import sys
import uuid
import zlib
from pathlib import Path

# Canvas + grid defaults (Q5: uniform grid, declaration order, tablet landscape).
CANVAS_W, CANVAS_H = 1024, 768
PAD = 12
LABEL_H = 28
DEFAULT_COLS = 4


# --- metadata from the committed schema -------------------------------------------------

def default_schema_path(script: Path) -> Path:
    """The committed schema, relative to this script inside the repo."""
    root = script.resolve().parents[3]  # .claude/skills/control-surface/ -> repo root
    return root / "crates" / "reuben-core" / "schema" / "instrument.schema.json"


def load_param_meta(schema: dict) -> dict:
    """type_name -> { param_name -> {min,max,default,unit,curve} }, parsed from the schema's
    per-type param branches (description is 'unit: X, curve: Y')."""
    out = {}
    for branch in schema["$defs"]["node"].get("allOf", []):
        type_name = branch["if"]["properties"]["type"]["const"]
        props = branch["then"]["properties"].get("params", {}).get("properties", {})
        params = {}
        for name, p in props.items():
            unit, curve = "", "Linear"
            for part in str(p.get("description", "")).split(","):
                part = part.strip()
                if part.startswith("unit:"):
                    unit = part[len("unit:"):].strip()
                elif part.startswith("curve:"):
                    curve = part[len("curve:"):].strip()
            params[name] = {
                "min": float(p.get("minimum", 0.0)),
                "max": float(p.get("maximum", 1.0)),
                "default": float(p.get("default", 0.0)),
                "unit": unit,
                "curve": curve,
            }
        out[type_name] = params
    return out


# --- control resolution -----------------------------------------------------------------

def node_param(node: dict, name: str, fallback: float) -> float:
    v = node.get("params", {}).get(name)
    return float(v) if v is not None else fallback


def map_inputs_connected(instrument: dict) -> set:
    """Addresses of `map` nodes whose message input is fed by an internal connection (so they
    are plumbing, not a public Good Button face)."""
    return {c["to"]["node"] for c in instrument.get("connections", [])}


def resolve_control(node: dict, spec: dict, meta: dict) -> dict:
    """Resolve one control spec on `node` into the concrete fields the emitter needs.

    `spec` is one `control` entry: {label, param?, unit?, widget?, min?, max?, default?}.
    No `param` => the value binds to the node address (a Good Button `map`); a `param` binds to
    `/<node>/<param>` and pulls range/unit/default from the param's schema metadata.
    """
    addr = node["address"]
    type_name = node["type"]
    label = spec["label"]
    widget = spec.get("widget", "fader")
    param = spec.get("param")

    if widget == "note-toggle":
        # A play toggle: fires `<node>/<port> [note, gate]` (e.g. /voicer/note). The note is a
        # constant so note-off carries the same MIDI as note-on and matches the held voice.
        port = spec.get("port", "note")
        return {
            "kind": "note-toggle", "label": label, "widget": widget,
            "osc_addr": f"{addr}/{port}", "note": float(spec.get("note", 60)),
        }

    if param is None:
        # Good Button: range is the map instance's input range; default is the map's resting
        # value (ADR-0018); the OSC address is the bare node address.
        lo = node_param(node, "in_min", 0.0)
        hi = node_param(node, "in_max", 1.0)
        default = node_param(node, "default", lo)
        unit = spec.get("unit", "")
        osc_addr = addr
    else:
        pm = meta.get(type_name, {}).get(param)
        if pm is None:
            raise SystemExit(
                f"node {addr}: control names param {param!r}, which {type_name!r} has no metadata for"
            )
        lo, hi, default = pm["min"], pm["max"], pm["default"]
        unit = spec.get("unit", pm["unit"])
        osc_addr = f"{addr}/{param}"

    # Per-spec overrides win over inferred range/default.
    lo = float(spec.get("min", lo))
    hi = float(spec.get("max", hi))
    default = float(spec.get("default", default))
    return {
        "kind": "fader", "label": label, "widget": widget, "osc_addr": osc_addr,
        "min": lo, "max": hi, "default": default, "unit": unit,
    }


def specs_of(node: dict):
    """A node's `control` value normalised to a list of specs (object or array both accepted)."""
    c = node.get("control")
    if c is None:
        return []
    return c if isinstance(c, list) else [c]


def collect_controls(instrument: dict, meta: dict) -> list:
    out = []
    for node in instrument["nodes"]:
        for spec in specs_of(node):
            out.append(resolve_control(node, spec, meta))
    return out


# --- inference (read-only candidate discovery) ------------------------------------------

def infer_candidates(instrument: dict, meta: dict) -> dict:
    connected = map_inputs_connected(instrument)
    good_buttons, params = [], []
    for node in instrument["nodes"]:
        addr, type_name = node["address"], node["type"]
        if type_name == "map" and addr not in connected:
            lo = node_param(node, "in_min", 0.0)
            hi = node_param(node, "in_max", 1.0)
            good_buttons.append({
                "address": addr, "binding": "good-button",
                "label_guess": addr.lstrip("/").replace("_", " ").title(),
                "widget": "fader", "min": lo, "max": hi,
                "default": node_param(node, "default", lo), "unit": "",
                "doc": node.get("doc", ""),
            })
        for pname, pm in meta.get(type_name, {}).items():
            params.append({
                "address": f"{addr}/{pname}", "binding": "param",
                "node": addr, "param": pname,
                "label_guess": pname.replace("_", " ").title(),
                "widget": "fader", "min": pm["min"], "max": pm["max"],
                "default": pm["default"], "unit": pm["unit"], "curve": pm["curve"],
            })
    return {"good_buttons": good_buttons, "params": params}


# --- .tosc emission ---------------------------------------------------------------------

def _id(*parts) -> str:
    """A stable UUID per element (deterministic output: same instrument -> same bytes)."""
    return str(uuid.uuid5(uuid.NAMESPACE_URL, "reuben-surface:" + ":".join(map(str, parts))))


# The emitter clones the structure of a known-good TouchOSC export (lexml version 6): typed
# <property> elements with CDATA keys, control values in <values>, and OSC messages whose
# <partial>/<trigger> use child elements (not attributes). Keys/strings are CDATA-wrapped to
# match the editor's output. Property sets per control type mirror the reference defaults so
# controls actually render — substituting only name/frame/text/value/message.

def _cd(s) -> str:
    """CDATA-wrap text the way TouchOSC wraps keys and string values; neutralise any `]]>`."""
    return "<![CDATA[" + str(s).replace("]]>", "]]]]><![CDATA[>") + "]]>"


def _num(v) -> str:
    if isinstance(v, bool):
        return "1" if v else "0"
    if isinstance(v, int):
        return str(v)
    return f"{v:g}"


def _p(type_code: str, key: str, body: str) -> str:
    return f"<property type='{type_code}'><key>{_cd(key)}</key><value>{body}</value></property>"


def _pb(k, v): return _p("b", k, "1" if v else "0")
def _pi(k, v): return _p("i", k, str(int(v)))
def _pf(k, v): return _p("f", k, _num(v))
def _ps(k, v): return _p("s", k, _cd(v))
def _pr(k, x, y, w, h): return _p("r", k, f"<x>{int(x)}</x><y>{int(y)}</y><w>{int(w)}</w><h>{int(h)}</h>")
def _pc(k, r, g, b, a): return _p("c", k, f"<r>{_num(r)}</r><g>{_num(g)}</g><b>{_num(b)}</b><a>{_num(a)}</a>")


def _value(key: str, default: str, locked_default_current: int = 0) -> str:
    return (f"<value><key>{_cd(key)}</key><locked>0</locked>"
            f"<lockedDefaultCurrent>{locked_default_current}</lockedDefaultCurrent>"
            f"<default>{_cd(default)}</default><defaultPull>0</defaultPull></value>")


def _partial(ptype: str, conversion: str, value, lo=0, hi=1) -> str:
    return (f"<partial><type>{ptype}</type><conversion>{conversion}</conversion>"
            f"<value>{_cd(value)}</value><scaleMin>{_num(lo)}</scaleMin>"
            f"<scaleMax>{_num(hi)}</scaleMax></partial>")


def _osc_msg(addr: str, arguments: list) -> str:
    """One-way OSC send to `addr` with the given argument partials. The path + trigger use child
    elements, matching the editor's export."""
    return (
        "<osc><enabled>1</enabled><send>1</send><receive>0</receive><feedback>0</feedback>"
        "<noDuplicates>0</noDuplicates><connections>1111111111</connections>"
        f"<triggers><trigger><var>{_cd('x')}</var><condition>ANY</condition></trigger></triggers>"
        f"<path>{_partial('CONSTANT', 'STRING', addr)}</path>"
        f"<arguments>{''.join(arguments)}</arguments></osc>"
    )


def _osc_fader(addr: str, lo: float, hi: float) -> str:
    """Fader send: `x` (0..1) scaled to the control's real range [lo,hi]."""
    return _osc_msg(addr, [_partial("VALUE", "FLOAT", "x", lo, hi)])


def _osc_note(addr: str, note: float) -> str:
    """Note toggle send: `<addr> [note, gate]` — a constant MIDI note plus the button's `x`
    (0/1) as velocity/gate. Constant note => note-off matches note-on."""
    return _osc_msg(addr, [_partial("CONSTANT", "FLOAT", _num(note)),
                           _partial("VALUE", "FLOAT", "x")])


def _group_props(w, h) -> str:
    return "".join([
        _pb("background", True), _pc("color", 0, 0, 0, 1), _pf("cornerRadius", 1),
        _pr("frame", 0, 0, w, h), _pb("grabFocus", False), _pb("interactive", False),
        _pb("locked", False), _pi("orientation", 0), _pb("outline", True),
        _pi("outlineStyle", 0), _pi("pointerPriority", 0), _pi("shape", 1), _pb("visible", True),
    ])


def _fader_props(name, frame) -> str:
    x, y, w, h = frame
    return "".join([
        _pb("background", True), _pb("bar", True), _pi("barDisplay", 0), _pc("color", 1, 0, 0, 1),
        _pf("cornerRadius", 1), _pb("cursor", True), _pi("cursorDisplay", 0), _pr("frame", x, y, w, h),
        _pb("grabFocus", True), _pb("grid", True), _pc("gridColor", 0, 0, 0, 0.25), _pi("gridSteps", 13),
        _pb("interactive", True), _pb("locked", False), _ps("name", name), _pi("orientation", 0),
        _pb("outline", True), _pi("outlineStyle", 1), _pi("pointerPriority", 0), _pi("response", 0),
        _pi("responseFactor", 100), _pi("shape", 1), _pb("visible", True),
    ])


def _button_props(name, frame) -> str:
    x, y, w, h = frame
    # buttonType 2 = Toggle Press (from the reference export).
    return "".join([
        _pb("background", True), _pi("buttonType", 2), _pc("color", 1, 0, 0, 1), _pf("cornerRadius", 1),
        _pr("frame", x, y, w, h), _pb("grabFocus", True), _pb("interactive", True), _pb("locked", False),
        _ps("name", name), _pi("orientation", 0), _pb("outline", True), _pi("outlineStyle", 1),
        _pi("pointerPriority", 0), _pb("press", True), _pb("release", True), _pi("shape", 1),
        _pb("valuePosition", False), _pb("visible", True),
    ])


def _label_props(name, frame) -> str:
    x, y, w, h = frame
    return "".join([
        _pb("background", True), _pc("color", 0, 0, 0, 0.25), _pf("cornerRadius", 1), _pi("font", 0),
        _pr("frame", x, y, w, h), _pb("grabFocus", True), _pb("interactive", False), _pb("locked", False),
        _ps("name", name), _pi("orientation", 0), _pb("outline", True), _pi("outlineStyle", 1),
        _pi("pointerPriority", 0), _pi("shape", 1), _pi("textAlignH", 1), _pi("textAlignV", 2),
        _pb("textClip", True), _pc("textColor", 1, 1, 1, 1), _pi("textLength", 0), _pi("textSize", 14),
        _pb("visible", True),
    ])


def _node(cid: str, ctype: str, props: str, values: str = "", messages: str = "") -> str:
    inner = f"<properties>{props}</properties>"
    if values:
        inner += f"<values>{values}</values>"
    if messages:
        inner += f"<messages>{messages}</messages>"
    return f"<node ID='{cid}' type='{ctype}'>{inner}</node>"


def build_tosc(instrument: dict, controls: list, cols: int) -> bytes:
    name = instrument.get("instrument", "surface")
    cols = max(1, min(cols, len(controls)))
    rows = max(1, (len(controls) + cols - 1) // cols)
    cell_w = (CANVAS_W - PAD * (cols + 1)) / cols
    cell_h = (CANVAS_H - PAD * (rows + 1)) / rows

    kids = []
    for i, c in enumerate(controls):
        col, row = i % cols, i // cols
        x = PAD + col * (cell_w + PAD)
        y = PAD + row * (cell_h + PAD)
        lframe = (int(x), int(y), int(cell_w), LABEL_H)
        wframe = (int(x), int(y + LABEL_H), int(cell_w), int(cell_h - LABEL_H))

        if c["kind"] == "note-toggle":
            caption = f"{c['label']} (note {int(c['note'])})"
            widget = _node(_id(name, c["osc_addr"], "widget"), "BUTTON",
                           _button_props(c["label"], wframe),
                           values=_value("x", "0") + _value("touch", "false"),
                           messages=_osc_note(c["osc_addr"], c["note"]))
        else:
            rng = f"{c['min']:g}–{c['max']:g}{(' ' + c['unit']) if c['unit'] else ''}"
            caption = f"{c['label']} ({rng})"
            span = c["max"] - c["min"]
            default_x = 0.0 if span == 0 else max(0.0, min(1.0, (c["default"] - c["min"]) / span))
            widget = _node(_id(name, c["osc_addr"], "widget"), "FADER",
                           _fader_props(c["label"], wframe),
                           values=_value("x", _num(default_x)) + _value("touch", "false"),
                           messages=_osc_fader(c["osc_addr"], c["min"], c["max"]))

        # A label above the widget so the surface reads its name (text lives in <values>).
        lvals = _value("text", caption, locked_default_current=1) + _value("touch", "false")
        kids.append(_node(_id(name, c["osc_addr"], "label"), "LABEL", _label_props(c["label"], lframe), values=lvals))
        kids.append(widget)

    root = (f"<node ID='{_id(name, 'root')}' type='GROUP'><includes/>"
            f"<properties>{_group_props(CANVAS_W, CANVAS_H)}</properties>"
            f"<values>{_value('touch', 'false')}</values>"
            f"<children>{''.join(kids)}</children></node>")
    xml = "<?xml version='1.0' encoding='UTF-8'?><lexml version='6'>" + root + "</lexml>"
    return zlib.compress(xml.encode("utf-8"))


# --- cli --------------------------------------------------------------------------------

def _read_json(p: Path) -> dict:
    return json.loads(p.read_text())


def main(argv=None):
    ap = argparse.ArgumentParser(description="Generate a TouchOSC surface from a reuben instrument.")
    sub = ap.add_subparsers(dest="cmd", required=True)
    script = Path(__file__)

    pi = sub.add_parser("infer", help="print control candidates as JSON (read-only)")
    pi.add_argument("instrument")
    pi.add_argument("--schema", default=None)

    pe = sub.add_parser("emit", help="emit a .tosc from the instrument's control blocks")
    pe.add_argument("instrument")
    pe.add_argument("--schema", default=None)
    pe.add_argument("--host", default="localhost")
    pe.add_argument("--port", type=int, default=9000)
    pe.add_argument("--out", default=None)
    pe.add_argument("--cols", type=int, default=DEFAULT_COLS)

    args = ap.parse_args(argv)
    inst_path = Path(args.instrument)
    instrument = _read_json(inst_path)
    schema_path = Path(args.schema) if args.schema else default_schema_path(script)
    meta = load_param_meta(_read_json(schema_path))

    if args.cmd == "infer":
        print(json.dumps(infer_candidates(instrument, meta), indent=2))
        return 0

    controls = collect_controls(instrument, meta)
    if not controls:
        sys.exit(f"{inst_path}: no `control` blocks found — run `infer` and add them first (ADR-0018).")
    # Generated surfaces live in the repo's versioned `control-surfaces/` dir (shareable).
    if args.out:
        out = Path(args.out)
    else:
        out = script.resolve().parents[3] / "control-surfaces" / f"{inst_path.stem}.tosc"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_bytes(build_tosc(instrument, controls, args.cols))
    print(f"wrote {out} — {len(controls)} control(s), send to {args.host}:{args.port}")
    print("NOTE: in TouchOSC, set the OSC connection host/port to the machine running reuben.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

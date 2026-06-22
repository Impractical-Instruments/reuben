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
from xml.etree import ElementTree as ET

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
        "label": label, "widget": widget, "osc_addr": osc_addr,
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


def _prop(parent: ET.Element, type_code: str, key: str, value):
    """Append a <property type=..><key/><value/></property>. Frame (r) and colour (c) nest
    their components under <value>; scalars put text directly. (The r/c nesting-vs-attribute
    form is the one format detail to confirm in the on-device verify pass.)"""
    p = ET.SubElement(parent, "property", {"type": type_code})
    ET.SubElement(p, "key").text = key
    v = ET.SubElement(p, "value")
    if type_code == "r":
        for k in ("x", "y", "w", "h"):
            ET.SubElement(v, k).text = str(value[k])
    elif type_code == "c":
        for k in ("r", "g", "b", "a"):
            ET.SubElement(v, k).text = str(value[k])
    else:
        v.text = str(value)


def _frame(x, y, w, h):
    return {"x": x, "y": y, "w": w, "h": h}


def _control_node(parent: ET.Element, ctype: str, cid: str, name: str, frame: dict):
    n = ET.SubElement(parent, "node", {"ID": cid, "type": ctype})
    props = ET.SubElement(n, "properties")
    _prop(props, "s", "name", name)
    _prop(props, "r", "frame", frame)
    return n


def _osc_send(node: ET.Element, osc_addr: str, lo: float, hi: float):
    """Attach a one-way OSC send: fader value x in [0,1] is scaled to [lo,hi] (real values,
    Q9) and sent to `osc_addr`."""
    msgs = ET.SubElement(node, "messages")
    osc = ET.SubElement(msgs, "osc")
    ET.SubElement(osc, "enabled").text = "1"
    ET.SubElement(osc, "send").text = "1"
    ET.SubElement(osc, "receive").text = "0"      # one-way (Q6)
    ET.SubElement(osc, "feedback").text = "0"
    ET.SubElement(osc, "connections").text = "00001"
    trg = ET.SubElement(osc, "triggers")
    ET.SubElement(trg, "trigger", {"var": "x", "con": "ANY"})
    path = ET.SubElement(osc, "path")
    ET.SubElement(path, "partial",
                  {"type": "CONSTANT", "conversion": "STRING", "value": osc_addr,
                   "scaleMin": "0", "scaleMax": "1"})
    args = ET.SubElement(osc, "arguments")
    ET.SubElement(args, "partial",
                  {"type": "VALUE", "conversion": "FLOAT", "value": "x",
                   "scaleMin": str(lo), "scaleMax": str(hi)})


def _fader_value(node: ET.Element, default_x: float):
    vals = ET.SubElement(node, "values")
    v = ET.SubElement(vals, "value")
    ET.SubElement(v, "key").text = "x"
    ET.SubElement(v, "locked").text = "0"
    ET.SubElement(v, "lockedDefaultCurrent").text = "0"
    ET.SubElement(v, "default").text = f"{default_x:.6g}"
    ET.SubElement(v, "defaultPull").text = "0"


def build_tosc(instrument: dict, controls: list, cols: int) -> bytes:
    name = instrument.get("instrument", "surface")
    lexml = ET.Element("lexml", {"version": "3"})
    root = ET.SubElement(lexml, "node", {"ID": _id(name, "root"), "type": "GROUP"})
    rprops = ET.SubElement(root, "properties")
    _prop(rprops, "s", "name", name)
    _prop(rprops, "r", "frame", _frame(0, 0, CANVAS_W, CANVAS_H))
    children = ET.SubElement(root, "children")

    cols = max(1, min(cols, len(controls)))
    rows = max(1, (len(controls) + cols - 1) // cols)
    cell_w = (CANVAS_W - PAD * (cols + 1)) / cols
    cell_h = (CANVAS_H - PAD * (rows + 1)) / rows

    for i, c in enumerate(controls):
        col, row = i % cols, i // cols
        x = PAD + col * (cell_w + PAD)
        y = PAD + row * (cell_h + PAD)
        # A label above the widget so the surface reads its name + range.
        rng = f"{c['min']:g}–{c['max']:g}{(' ' + c['unit']) if c['unit'] else ''}"
        _control_node(children, "LABEL", _id(name, c["osc_addr"], "label"),
                      f"{c['label']} ({rng})", _frame(int(x), int(y), int(cell_w), LABEL_H))

        wtype = {"fader": "FADER", "button": "BUTTON", "label": "LABEL"}.get(c["widget"], "FADER")
        wn = _control_node(children, wtype, _id(name, c["osc_addr"], "widget"),
                           c["label"],
                           _frame(int(x), int(y + LABEL_H), int(cell_w), int(cell_h - LABEL_H)))
        _osc_send(wn, c["osc_addr"], c["min"], c["max"])
        if wtype == "FADER":
            span = c["max"] - c["min"]
            default_x = 0.0 if span == 0 else max(0.0, min(1.0, (c["default"] - c["min"]) / span))
            _fader_value(wn, default_x)

    xml = b'<?xml version="1.0" encoding="UTF-8"?>\n' + ET.tostring(lexml, encoding="utf-8")
    return zlib.compress(xml)


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
    out = Path(args.out) if args.out else inst_path.with_suffix(".tosc")
    out.write_bytes(build_tosc(instrument, controls, args.cols))
    print(f"wrote {out} — {len(controls)} control(s), send to {args.host}:{args.port}")
    print("NOTE: in TouchOSC, set the OSC connection host/port to the machine running reuben.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Generate a Hexler TouchOSC control surface (.tosc) from a reuben instrument (ADR-0018).

The deterministic half of the `control-surface` skill: the agent decides *which* controls a
surface should have and labels them (writing `control` blocks into the instrument JSON); this
script does the mechanical, repeatable parts — resolving each control's OSC address + value
range from authoritative metadata, and emitting the gzip/zlib-packed XML TouchOSC reads.

Three subcommands:

  infer  INSTRUMENT [--schema P]
      Read-only. Print JSON describing every control *candidate* — externally-driven Good
      Buttons (a `map` whose input is not wired from another node) and each settable `Float`
      input, with its resolved address, range, unit and default. The agent curates this into
      `control` blocks. Nothing is written.

  emit   INSTRUMENT [--schema P] [--host H] [--port N] [--out F] [--cols N]
      Read-only on the instrument. Resolve every node that carries a `control` block and write
      a `.tosc` surface targeting `host:port`.

  boundary INSTRUMENT [--describe F] [--reuben P] [--host H] [--port N] [--out F] [--cols N]
      Read-only on the instrument. For a *nested* instrument with a curated `interface`
      (ADR-0034 §4), emit one fader per wireable boundary input straight from the interface —
      no `control` blocks needed, the boundary *is* the curated set. Metadata (kind, effective
      default, min/max, unit, curve, label, widget, `driven`) is read from `reuben describe
      --json` (core `describe_boundary`, so this never re-implements the inherit+override merge);
      the OSC address is the entry's inner target, which stays reachable (ADR-0034 §3).

Metadata is read from the committed instrument schema (the per-type param ranges + unit/curve),
which is kept in sync with the operator descriptors by the `committed_schema_is_in_sync` test — so this
script never re-implements operator metadata, it reads the single source of truth.

OSC addressing (verified against the core router, ADR-0011):
  - Good Button (a `map` front-end): the widget sends to the node address, e.g. `/brightness`.
    Range = the node's `in_min`/`in_max` *instance* input values (default 0..1).
  - Direct input:                    the widget sends to `/<node>/<input>`, e.g. `/clock/tempo`
    or `/filter/cutoff`. Range = the input's schema min/max; unit/default likewise. A `Float`
    input is settable directly now (ADR-0028) — no `m2s` front-end needed to reach it.
"""

import argparse
import json
import subprocess
import sys
import uuid
import zlib
from pathlib import Path

# Canvas + grid defaults (Q5: uniform grid, declaration order, tablet landscape).
CANVAS_W, CANVAS_H = 1024, 768
PAD = 12
LABEL_H = 28
DEFAULT_COLS = 4
STEP_COLS = 16  # a gate-step lane lays out as a full-width row, wrapping past this many buttons


# --- metadata from the committed schema -------------------------------------------------

def default_schema_path(script: Path) -> Path:
    """The committed schema, relative to this script inside the repo."""
    root = script.resolve().parents[3]  # .claude/skills/control-surface/ -> repo root
    return root / "crates" / "reuben-core" / "schema" / "instrument.schema.json"


def _number_form(prop: dict) -> dict | None:
    """The numeric (fader-settable) form of an `inputs` property's schema, or `None` (ADR-0028).

    An input is either a plain number schema or a `oneOf` of forms — a settable `Float` carries a
    `{"type": "number", ...}` member with its min/max/default/description. A bare-`float` audio
    input (only a wire-ref form) and an `Enum` input (a string-enum form) have no number form, so
    they are not faders and return `None`."""
    if prop.get("type") == "number":
        return prop
    for form in prop.get("oneOf", []):
        if form.get("type") == "number":
            return form
    return None


def load_param_meta(schema: dict) -> dict:
    """type_name -> { input_name -> {min,max,default,unit,curve} } for each settable `Float`
    input, parsed from the schema's per-type `inputs` branches (ADR-0028; description is
    'unit: X, curve: Y'). `config` Constants and non-numeric inputs (audio/Enum) are skipped."""
    out = {}
    for branch in schema["$defs"]["node"].get("allOf", []):
        type_name = branch["if"]["properties"]["type"]["const"]
        props = branch["then"]["properties"].get("inputs", {}).get("properties", {})
        params = {}
        for name, prop in props.items():
            p = _number_form(prop)
            if p is None:
                continue  # audio passthrough or an Enum input — not a fader control
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
    """A node's literal `inputs[name]` value as a float (ADR-0028). A wire-ref (`{"from": ...}`)
    or an Enum symbol is not numeric, so it falls back."""
    v = node.get("inputs", {}).get(name)
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        return fallback
    return float(v)


def map_inputs_connected(instrument: dict) -> set:
    """Addresses of nodes whose `inputs` include a wire-ref (ADR-0028: wiring lives in each
    node's `inputs` as `{"from": "/src.port"}`, not a top-level `connections` array). A `map`
    whose input is wired is internal plumbing, not a public Good Button face."""
    fed = set()
    for node in instrument.get("nodes", []):
        if any(isinstance(v, dict) and "from" in v for v in node.get("inputs", {}).values()):
            fed.add(node["address"])
    return fed


def is_gate_step(node: dict, param) -> bool:
    """True when `param` is a `stepN` on a sequencer running `gate_mode`=1 — i.e. a boolean
    on/off step (ADR-0022), not a continuous degree. Such a step is faithfully a toggle button,
    not a fader: the button's `x` (0/1) is exactly the param's domain."""
    if node.get("type") != "sequencer" or not param:
        return False
    if not (param.startswith("step") and param[4:].isdigit()):
        return False
    return node_param(node, "gate_mode", 0.0) == 1.0


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
        # A play toggle: fires `<node>/<port> [note, gate]` (e.g. /voicer/notes). The note is a
        # constant so note-off carries the same MIDI as note-on and matches the held voice.
        port = spec.get("port", "notes")
        return {
            "kind": "note-toggle", "label": label, "widget": widget,
            "osc_addr": f"{addr}/{port}", "note": float(spec.get("note", 60)),
        }

    if widget == "param-toggle" or is_gate_step(node, param):
        # A boolean step button: sends `<node>/<param> [x]` where `x` is the button's own 0/1.
        # Unlike note-toggle (a constant note to a *port*, where the value is velocity), the
        # button value IS the payload set onto the param — so on->off carries through. Default
        # = the step's resting param value in the instrument, so the surface mirrors the pattern.
        if param is None:
            raise SystemExit(f"node {addr}: param-toggle control {label!r} needs a `param`")
        return {
            "kind": "param-toggle", "label": label, "widget": "param-toggle",
            "osc_addr": f"{addr}/{param}", "node": addr,
            "default": float(spec.get("default", node_param(node, param, 0.0))),
        }

    if widget == "chord-button":
        # A chord button (V1.3 Chord-player, ADR-0022): a toggle that fires `<node>/<port>
        # [degree, gate]` — a constant scale **degree** (the chord root) plus the button's gate.
        # Same custom-2-arg-payload mechanism as `note-toggle`, but the constant rides as a scale
        # degree (resolved downstream by the `chord` op + voicer), not an absolute MIDI note; the
        # constant degree means the release carries the same root and stops the held chord.
        port = spec.get("port", "set")
        return {
            "kind": "chord-button", "label": label, "widget": widget,
            "osc_addr": f"{addr}/{port}", "degree": float(spec.get("degree", 0)),
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
            c = resolve_control(node, spec, meta)
            # `group` is a layout hint, not a binding: consecutive controls sharing it pack onto
            # one row (see layout_rows). Carried straight through from the spec.
            if spec.get("group") is not None:
                c["group"] = spec["group"]
            out.append(c)
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


# --- interface boundary (nested instruments, ADR-0034 §4) -------------------------------

# The boundary port kinds that map to a fader: a held scalar knob (`value`), a swept scalar
# (`signal`), or a ranged integer (`int`). `reuben describe --json` reports the kind (issue #176:
# a held `f32` Value is `value`, a dense `f32_buffer` Signal is `signal`). Everything else — a
# bare audio buffer (a `signal` with no range), an `enum` (needs a selector widget), a
# `message`/`harmony`/`arg`/`string` — is not a fader and is skipped (matching the operator-param
# scope: enums out today).
FADER_BOUNDARY_KINDS = {"value", "signal", "int"}


def _osc_from_target(target: str) -> str:
    """The OSC address an `interface` entry's internal `/node.port` target is reachable at.
    ADR-0034 §3 keeps a nested instrument's inner addresses OSC-reachable; the port separator
    `.` becomes `/`, yielding exactly the `/<node>/<input>` shape a direct input already uses
    (e.g. `/filter.cutoff` -> `/filter/cutoff`)."""
    return target.replace(".", "/", 1)


def _entry_target(entry) -> str | None:
    """The internal target of one `interface.inputs` entry: a bare `"/node.port"` string, or the
    `target` field of an override object (ADR-0034 §4). `None` if neither is present."""
    if isinstance(entry, str):
        return entry
    if isinstance(entry, dict):
        return entry.get("target")
    return None


def boundary_controls(interface_inputs: dict, boundary: dict) -> list:
    """One fader control per wireable `interface` input, resolved from a `reuben describe --json`
    boundary view (ADR-0034 §4). The boundary is already the curated presentational metadata, so
    unlike `collect_controls` this authors no `control` blocks — the interface *is* the surface.

    `interface_inputs` is the instrument's `interface.inputs` map (name -> target string or
    override object), read straight from the document for each entry's inner target (the OSC
    address; `describe` deliberately omits it). `boundary` is the parsed `describe --json` output,
    the source of every port's kind/default/min/max/unit/curve/label/widget/`driven`.

    Skips inputs a host cannot drive from a fader: a `driven` input (its inner Signal port is
    already wired inside the patch, so an external wire is the fatal `BoundaryInputDriven`), a
    non-fader kind, and a range-less numeric (bare audio, no min/max to scale into)."""
    controls = []
    for port in boundary.get("inputs", []):
        name = port.get("name")
        if port.get("driven"):
            continue
        if port.get("kind") not in FADER_BOUNDARY_KINDS:
            continue
        if port.get("min") is None or port.get("max") is None:
            continue  # a bare audio buffer (undriven, but unranged) — not a fader
        target = _entry_target(interface_inputs.get(name))
        if not target:
            continue  # no way to address it (dark entry / malformed) — skip rather than misfire
        lo, hi = float(port["min"]), float(port["max"])
        default = float(port["default"]) if port.get("default") is not None else lo
        controls.append({
            "kind": "fader",
            "label": port.get("label") or str(name).replace("_", " ").title(),
            "widget": port.get("widget") or "fader",
            "osc_addr": _osc_from_target(target),
            "min": lo, "max": hi, "default": default,
            "unit": port.get("unit", ""),
        })
    return controls


def find_reuben(script: Path, override: str | None) -> str:
    """The `reuben` binary to run `describe` with: an explicit `--reuben` path, else the built
    binary under the repo's `target/{release,debug}`, else `reuben` on `PATH`."""
    if override:
        return override
    root = script.resolve().parents[3]  # .claude/skills/control-surface/ -> repo root
    for profile in ("release", "debug"):
        cand = root / "target" / profile / "reuben"
        if cand.exists():
            return str(cand)
    return "reuben"


def run_describe(reuben_bin: str, inst_path: Path) -> dict:
    """Invoke `reuben describe <instrument> --json` and parse its boundary view. Raises with the
    binary's own stderr on failure so a bad patch reads the same as it would on the CLI."""
    try:
        proc = subprocess.run(
            [reuben_bin, "describe", str(inst_path), "--json"],
            capture_output=True, text=True,
        )
    except FileNotFoundError:
        raise SystemExit(
            f"could not run {reuben_bin!r} — build it (`cargo build --bin reuben`) or pass "
            f"`--reuben PATH`, or feed a pre-captured `--describe FILE`."
        )
    if proc.returncode != 0:
        raise SystemExit(f"`reuben describe {inst_path}` failed: {proc.stderr.strip()}")
    return json.loads(proc.stdout)


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


def _osc_param_toggle(addr: str) -> str:
    """Toggle button -> param: the button's `x` (0/1) is the sole arg, set straight onto
    `<node>/<param>`. No scaling — a gate step's domain is already [0,1]."""
    return _osc_msg(addr, [_partial("VALUE", "FLOAT", "x")])

def _osc_chord(addr: str, degree: float) -> str:
    """Chord button send: `<addr> [degree, gate]` — a constant scale **degree** (the chord
    root) plus the button's `x` (0/1) as gate. Constant degree => release stops the same
    held chord. Same 2-arg shape as `_osc_note`, but the constant is a degree, not a MIDI
    note (the `chord` op stacks thirds; the voicer resolves degrees through the context)."""
    return _osc_msg(addr, [_partial("CONSTANT", "FLOAT", _num(degree)),
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


def _radial_props(name, frame) -> str:
    x, y, w, h = frame
    # RADIAL = a rotary fader (same single `x` value model as FADER, ADR-0018). Property keys +
    # order are cloned from the reference export's RADIAL: it drops the fader's bar/cursor keys and
    # adds `centered`/`inverted`; shape 2 renders the knob. Keep this key set + order in lockstep
    # with fixtures/REUBEN_REF.tosc (FixtureMatchTest asserts it).
    return "".join([
        _pb("background", True), _pb("centered", False), _pc("color", 1, 0, 0, 1),
        _pf("cornerRadius", 1), _pr("frame", x, y, w, h), _pb("grabFocus", True), _pb("grid", True),
        _pc("gridColor", 0, 0, 0, 0.25), _pi("gridSteps", 13), _pb("interactive", True),
        _pb("inverted", False), _pb("locked", False), _ps("name", name), _pi("orientation", 0),
        _pb("outline", True), _pi("outlineStyle", 1), _pi("pointerPriority", 0), _pi("response", 0),
        _pi("responseFactor", 100), _pi("shape", 2), _pb("visible", True),
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


def layout_rows(controls: list, cols: int) -> list:
    """Pack controls into rows of varying width, preserving declaration order. Three cases:
      - a run of consecutive `param-toggle` controls sharing one node (a sequencer lane) becomes
        its own row, wrapping at STEP_COLS;
      - a run of consecutive controls sharing a `group` hint becomes its own row (e.g. a drum
        channel's knobs), wrapping at STEP_COLS — this is how a caller lays one logical group per
        row regardless of the default column count;
      - everything else flows into uniform rows of `cols`.
    So a lane's 16 steps line up as one row, each `group` gets its own row, and the misc controls
    (tempo, volumes, tone) grid up in between."""
    rows, grid = [], []

    def flush_grid():
        for k in range(0, len(grid), cols):
            rows.append(grid[k:k + cols])
        grid.clear()

    def emit_run(run):
        for k in range(0, len(run), STEP_COLS):
            rows.append(run[k:k + STEP_COLS])

    i, n = 0, len(controls)
    while i < n:
        c = controls[i]
        if c["kind"] == "param-toggle":
            flush_grid()
            node, j = c["node"], i
            while j < n and controls[j]["kind"] == "param-toggle" and controls[j]["node"] == node:
                j += 1
            emit_run(controls[i:j])
            i = j
        elif c.get("group") is not None:
            flush_grid()
            grp, j = c["group"], i
            while j < n and controls[j].get("group") == grp and controls[j]["kind"] != "param-toggle":
                j += 1
            emit_run(controls[i:j])
            i = j
        else:
            grid.append(c)
            i += 1
    flush_grid()
    return rows


def _widget_node(name: str, c: dict, wframe) -> tuple:
    """Build a control's widget <node> and its label caption."""
    if c["kind"] == "note-toggle":
        caption = f"{c['label']} (note {int(c['note'])})"
        return caption, _node(_id(name, c["osc_addr"], "widget"), "BUTTON",
                              _button_props(c["label"], wframe),
                              values=_value("x", "0") + _value("touch", "false"),
                              messages=_osc_note(c["osc_addr"], c["note"]))
    if c["kind"] == "chord-button":
        caption = f"{c['label']} (deg {int(c['degree'])})"
        return caption, _node(_id(name, c["osc_addr"], "widget"), "BUTTON",
                              _button_props(c["label"], wframe),
                              values=_value("x", "0") + _value("touch", "false"),
                              messages=_osc_chord(c["osc_addr"], c["degree"]))
    if c["kind"] == "param-toggle":
        return c["label"], _node(_id(name, c["osc_addr"], "widget"), "BUTTON",
                                 _button_props(c["label"], wframe),
                                 values=_value("x", _num(c["default"])) + _value("touch", "false"),
                                 messages=_osc_param_toggle(c["osc_addr"]))
    rng = f"{c['min']:g}–{c['max']:g}{(' ' + c['unit']) if c['unit'] else ''}"
    span = c["max"] - c["min"]
    default_x = 0.0 if span == 0 else max(0.0, min(1.0, (c["default"] - c["min"]) / span))
    # `radial` renders the same value as a rotary knob; everything else (default, OSC scaling) is
    # identical to a fader, so only the node type + property block differ.
    if c.get("widget") == "radial":
        ctype, props = "RADIAL", _radial_props(c["label"], wframe)
    else:
        ctype, props = "FADER", _fader_props(c["label"], wframe)
    return f"{c['label']} ({rng})", _node(_id(name, c["osc_addr"], "widget"), ctype, props,
                                          values=_value("x", _num(default_x)) + _value("touch", "false"),
                                          messages=_osc_fader(c["osc_addr"], c["min"], c["max"]))


def build_tosc(instrument: dict, controls: list, cols: int) -> bytes:
    name = instrument.get("instrument", "surface")
    cols = max(1, cols)
    rows = layout_rows(controls, cols)
    nrows = max(1, len(rows))
    cell_h = (CANVAS_H - PAD * (nrows + 1)) / nrows

    kids = []
    for r, row in enumerate(rows):
        ncol = max(1, len(row))
        cell_w = (CANVAS_W - PAD * (ncol + 1)) / ncol
        y = PAD + r * (cell_h + PAD)
        for col, c in enumerate(row):
            x = PAD + col * (cell_w + PAD)
            lframe = (int(x), int(y), int(cell_w), LABEL_H)
            wx, wy, ww, wh = x, y + LABEL_H, cell_w, cell_h - LABEL_H
            if c.get("widget") == "radial":
                # A RADIAL renders a circle sized to its frame's bounding box — give it the full
                # (wide, short) fader cell and the knob overflows into the neighbouring rows. Box it
                # into the largest centred square that fits the cell so each knob stays put.
                side = min(ww, wh)
                wx += (ww - side) / 2
                wy += (wh - side) / 2
                ww = wh = side
            wframe = (int(wx), int(wy), int(ww), int(wh))
            caption, widget = _widget_node(name, c, wframe)
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


def _surface_out(script: Path, inst_path: Path, override) -> Path:
    """Where the `.tosc` lands: an explicit `--out`, else the repo's versioned, shareable
    `control-surfaces/<instrument-stem>.tosc`."""
    out = Path(override) if override else script.resolve().parents[3] / "control-surfaces" / f"{inst_path.stem}.tosc"
    out.parent.mkdir(parents=True, exist_ok=True)
    return out


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

    pb = sub.add_parser("boundary",
                        help="emit a .tosc from a nested instrument's `interface` boundary")
    pb.add_argument("instrument")
    pb.add_argument("--describe", default=None,
                    help="pre-captured `reuben describe --json` output (skips running the binary)")
    pb.add_argument("--reuben", default=None, help="path to the `reuben` binary")
    pb.add_argument("--host", default="localhost")
    pb.add_argument("--port", type=int, default=9000)
    pb.add_argument("--out", default=None)
    pb.add_argument("--cols", type=int, default=DEFAULT_COLS)

    args = ap.parse_args(argv)
    inst_path = Path(args.instrument)
    instrument = _read_json(inst_path)

    if args.cmd == "boundary":
        # The boundary is served, already merged, by core `describe_boundary` (ADR-0034 §4) — so
        # this path reads `describe --json`, not the committed operator schema.
        boundary = _read_json(Path(args.describe)) if args.describe \
            else run_describe(find_reuben(script, args.reuben), inst_path)
        iface = (instrument.get("interface") or {}).get("inputs", {})
        controls = boundary_controls(iface, boundary)
        if not controls:
            sys.exit(f"{inst_path}: no wireable `interface` inputs to surface — needs a nested "
                     f"instrument with a curated boundary (ADR-0034 §4).")
        out = _surface_out(script, inst_path, args.out)
        out.write_bytes(build_tosc(instrument, controls, args.cols))
        print(f"wrote {out} — {len(controls)} control(s) from the interface boundary, "
              f"send to {args.host}:{args.port}")
        print("NOTE: in TouchOSC, set the OSC connection host/port to the machine running reuben.")
        return 0

    schema_path = Path(args.schema) if args.schema else default_schema_path(script)
    meta = load_param_meta(_read_json(schema_path))

    if args.cmd == "infer":
        print(json.dumps(infer_candidates(instrument, meta), indent=2))
        return 0

    controls = collect_controls(instrument, meta)
    if not controls:
        sys.exit(f"{inst_path}: no `control` blocks found — run `infer` and add them first (ADR-0018).")
    out = _surface_out(script, inst_path, args.out)
    out.write_bytes(build_tosc(instrument, controls, args.cols))
    print(f"wrote {out} — {len(controls)} control(s), send to {args.host}:{args.port}")
    print("NOTE: in TouchOSC, set the OSC connection host/port to the machine running reuben.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

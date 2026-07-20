#!/usr/bin/env python3
"""Generate a Hexler TouchOSC control surface (.tosc) for a reuben instrument.

The deterministic half of the `control-surface` skill: a *surface doc* (`surfaces/*.json`,
per `surfaces/surface.schema.json`) binds interface input pipes by name and carries the
presentation (label/widget/group/order + an optional narrower range); the instrument's
`interface.inputs` pipes carry the quantity contract (type/min/max/default/unit/curve).
This script resolves the two into a concrete widget list and projects it to the `.tosc`
XML TouchOSC reads. The resolver semantics are shared verbatim with the web player's JS
twin, which now lives in the private `reuben-web` repo and reads this repo through its
`engine/` submodule.

Two subcommands:

  emit   INSTRUMENT [--surface FILE] [--target touchosc] [--host H] [--port N] [--out F] [--cols N]
      Read-only on the instrument. Resolve the surface doc — an explicit `--surface`, else
      `surfaces/<stem>.touchosc.json` ?? `surfaces/<stem>.json` ?? a default surface derived
      from the instrument's wireable input pipes — against the instrument's
      `interface.inputs`, and write a `.tosc` targeting `host:port`. Controls the TouchOSC
      target cannot render (reserved/web-only widget kinds, unknown binds, payload-less
      message pipes) are skipped loudly: one stderr warning naming each.

  boundary INSTRUMENT [--describe F] [--reuben P] [--host H] [--port N] [--out F] [--cols N]
      Read-only on the instrument. For an instrument whose `interface.inputs` declares control
      pipes, emit one fader per wireable input pipe straight from the *live
      engine's* view — metadata is read from `reuben describe --json`, so this path doubles as
      the drift guard binding the skill to the real describe contract. The OSC address is the
      pipe's `/<name>/in` port (the direction flip: the pipe mints its own address and
      fans out to internal consumers, so a fader drives the pipe, not an inner port).

OSC addressing: a control bound to pipe `name` sends to `/<name>/in` — the pipe
node minted at `/<name>`, its `in` port — the same address `describe` and the `boundary`
subcommand use. Note pipes take a `[note|degree, gate]` payload at that same port.
"""

# see rules: authoring-library

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

# The widget vocabulary this target renders (the shipped kinds). Everything else in
# the superset vocabulary — the reserved/web-only kinds ("xy-pad", "grid", "visualizer",
# "keyboard") and any unknown name — is skipped loudly by the resolver.
SHIPPED_WIDGETS = {"fader", "radial", "param-toggle", "note-toggle", "chord-button"}

# Widget kinds that ARE their own resolution kind; everything else ("fader", "radial", and any
# future fader-shaped widget) resolves through the fader path (kind "fader", widget verbatim).
TOGGLE_KINDS = {"param-toggle", "note-toggle", "chord-button"}


# --- surface-doc resolution ---------------------------------------------------------------

def interface_pipes(instrument: dict) -> dict:
    """`name -> pipe spec` for the instrument's interface *input pipes*: the `interface.inputs`
    entries carrying a `type` key (the quantity contract: type/default/min/max/curve/unit, plus
    an optional device `channel` binding). Anything without a `type` is not an input pipe."""
    inputs = instrument.get("interface", {}).get("inputs", {})
    return {name: spec for name, spec in inputs.items()
            if isinstance(spec, dict) and "type" in spec}


def default_label(name: str) -> str:
    """The pinned default-label algorithm (shared with the JS twin): replace "_" with " ", split
    on spaces, uppercase the first character of each word — "kick_step1" -> "Kick Step1".
    Deliberately NOT str.title(): title() also uppercases a letter following a digit
    ("mix2out" -> "Mix2Out"), which the shared semantics do not."""
    words = str(name).replace("_", " ").split(" ")
    return " ".join(w[:1].upper() + w[1:] for w in words)


def infer_widget(pipe: dict) -> str | None:
    """The default widget for a pipe with no explicit `widget`, or `None` when no
    default exists. A held scalar (`f32`), a held integer (`i32`), or a *ranged* dense
    signal (`f32_buffer` with declared min & max) backs a fader; a channel-bound pipe (a device
    binding, not a control), a bare `f32_buffer` (no range to scale into), and a message pipe
    (`note`/`harmony`/enum — a default cannot guess its payload) infer nothing."""
    if pipe.get("channel") is not None:
        return None
    t = pipe.get("type")
    if t in ("f32", "i32"):
        return "fader"
    if t == "f32_buffer" and pipe.get("min") is not None and pipe.get("max") is not None:
        return "fader"
    return None


def derive_surface(instrument: dict, instrument_name: str) -> tuple:
    """Synthesize the default surface doc from the instrument's pipes: one fader
    per wireable input pipe, declaration order (JSON object order). Pipes a default cannot
    surface are skipped with a warning naming each. Returns `(surface_doc, warnings)`."""
    controls, warnings = [], []
    for name, pipe in interface_pipes(instrument).items():
        if pipe.get("channel") is not None:
            warnings.append(f"default surface skips channel-bound pipe {name!r} "
                            f"(a device binding, not a control)")
            continue
        if infer_widget(pipe) == "fader":
            controls.append({"bind": name, "widget": "fader"})
        elif pipe.get("type") == "f32_buffer":
            warnings.append(f"default surface skips bare f32_buffer pipe {name!r} "
                            f"(no min/max to scale a fader into)")
        else:
            warnings.append(f"default surface skips {pipe.get('type')!r} pipe {name!r} "
                            f"(a default surface cannot guess its payload)")
    surface = {"surface_version": 1, "instrument": instrument_name, "controls": controls}
    return surface, warnings


def _clamp(v, lo, hi):
    """`v` clamped into `[lo, hi]`, either bound absent (`None`) meaning unbounded."""
    if lo is not None and v < lo:
        return lo
    if hi is not None and v > hi:
        return hi
    return v


def resolve_surface(instrument: dict, surface: dict, instrument_name: str | None = None) -> tuple:
    """Resolve a surface doc against the instrument's interface pipes into the concrete widget
    list the emitter (and the JS twin) consume. Returns `(widgets, warnings)`.

    Pinned shared semantics (the JS twin implements these identically):
      - `surface_version` != 1 is a hard error; an instrument-name mismatch is a warning only.
      - Each `controls[]` entry resolves in order. A `bind` naming no pipe warns and skips.
      - label = control.label ?? the pinned default-label algorithm on the pipe name.
      - widget = control.widget ?? the type-inferred widget; a message pipe with no explicit
        widget warns and skips. A reserved/web-only/unknown widget kind warns and skips (the
        TouchOSC skip table).
      - kind = widget for the toggle kinds, else "fader" (a "radial" is kind "fader").
      - min/max = control.min/max clamped INTO the pipe range (warn on an out-of-range
        override; absent -> the pipe's). default = the pipe default clamped into the effective
        [min, max]; a pipe without a default rests at min. unit/curve come from the PIPE only;
        `group` from the control. Keys are omitted when absent.
      - OSC address = `/<bind>/in`. note-toggle payload: `note` (required) + `velocity`
        (?? 1.0); chord-button payload: `degree` (required). Several controls may bind one pipe.
    """
    version = surface.get("surface_version")
    if version != 1:
        raise SystemExit(f"surface doc declares surface_version {version!r} — this resolver "
                         f"knows version 1 only")
    warnings = []
    declared = surface.get("instrument")
    if instrument_name is not None and declared is not None and declared != instrument_name:
        warnings.append(f"surface doc names instrument {declared!r} but is resolving against "
                        f"{instrument_name!r}")

    pipes = interface_pipes(instrument)
    widgets = []
    for control in surface.get("controls", []):
        bind = control.get("bind")
        pipe = pipes.get(bind)
        if pipe is None:
            warnings.append(f"surface control binds unknown pipe {bind!r} — skipped")
            continue
        label = control.get("label")
        if label is None:
            label = default_label(bind)
        widget = control.get("widget")
        if widget is None:
            widget = infer_widget(pipe)
            if widget is None:
                warnings.append(f"surface control {label!r} binds pipe {bind!r} (type "
                                f"{pipe.get('type')!r}) with no widget, and no widget can be "
                                f"inferred for that pipe type — skipped")
                continue
        if widget not in SHIPPED_WIDGETS:
            warnings.append(f"surface control {label!r} (widget {widget!r}) is not renderable "
                            f"on this target — skipped")
            continue
        kind = widget if widget in TOGGLE_KINDS else "fader"

        # Presentation range: the control may narrow the pipe range, never widen it (the
        # subset law) — an out-of-range override clamps into the pipe range, loudly.
        pipe_lo, pipe_hi = pipe.get("min"), pipe.get("max")
        lo = control.get("min")
        if lo is None:
            lo = pipe_lo
        else:
            clamped = _clamp(lo, pipe_lo, pipe_hi)
            if clamped != lo:
                warnings.append(f"surface control {label!r}: min {lo} is outside the pipe "
                                f"range [{pipe_lo}, {pipe_hi}] — clamped to {clamped}")
                lo = clamped
        hi = control.get("max")
        if hi is None:
            hi = pipe_hi
        else:
            clamped = _clamp(hi, pipe_lo, pipe_hi)
            if clamped != hi:
                warnings.append(f"surface control {label!r}: max {hi} is outside the pipe "
                                f"range [{pipe_lo}, {pipe_hi}] — clamped to {clamped}")
                hi = clamped
        default = pipe.get("default")
        default = lo if default is None else _clamp(default, lo, hi)

        c = {"kind": kind, "widget": widget, "bind": bind,
             "address": f"/{bind}/in", "label": label}
        # An integer control detents onto whole values: the fader/radial grid marks
        # each integer in [lo, hi] so the knob reads as stepped. The engine quantizes regardless
        # (the i32 pipe rounds live input), so this is presentation — a continuous drag still
        # lands on an integer at the pipe.
        if pipe.get("type") == "i32" and lo is not None and hi is not None:
            span = int(round(hi)) - int(round(lo))
            if span > 0:
                c["steps"] = span
        if control.get("group") is not None:
            c["group"] = control["group"]
        if lo is not None:
            c["min"] = lo
        if hi is not None:
            c["max"] = hi
        if default is not None:
            c["default"] = default
        if pipe.get("unit") is not None:
            c["unit"] = pipe["unit"]
        if pipe.get("curve") is not None:
            c["curve"] = pipe["curve"]
        if kind == "note-toggle":
            if control.get("note") is None:
                warnings.append(f"note-toggle {label!r} (pipe {bind!r}) is missing its required "
                                f"`note` payload — skipped")
                continue
            c["note"] = control["note"]
            c["velocity"] = control.get("velocity", 1.0)
        elif kind == "chord-button":
            if control.get("degree") is None:
                warnings.append(f"chord-button {label!r} (pipe {bind!r}) is missing its required "
                                f"`degree` payload — skipped")
                continue
            c["degree"] = control["degree"]
        widgets.append(c)
    return widgets, warnings


def find_surface_doc(surfaces_dir: Path, stem: str, target: str = "touchosc") -> Path | None:
    """The authored surface doc for instrument `stem`, per the resolution order:
    `surfaces/<stem>.<target>.json` ?? `surfaces/<stem>.json` ?? `None` (auto-derive)."""
    for cand in (surfaces_dir / f"{stem}.{target}.json", surfaces_dir / f"{stem}.json"):
        if cand.exists():
            return cand
    return None


# --- interface boundary (interface input pipes) -----------------------------------------

# The boundary port kinds that map to a fader: a held scalar knob (`value`), a swept scalar
# (`signal`), or a ranged integer (`int`). `reuben describe --json` reports the kind (issue #176:
# a held `f32` Value is `value`, a dense `f32_buffer` Signal is `signal`). Everything else — a
# bare audio buffer (a `signal` with no range), an `enum` (needs a selector widget), a
# `message`/`harmony`/`arg`/`string` — is not a fader and is skipped (matching the operator-param
# scope: enums out today).
FADER_BOUNDARY_KINDS = {"value", "signal", "int"}


def _osc_from_pipe(name: str) -> str:
    """The OSC address an `interface.inputs` pipe is driven at — the authoritative account of the
    addressing contract this whole boundary path rests on. An input pipe mints an
    address in the flat node namespace (`tone` -> node `/tone`) and takes external control on its
    single `in` port, so the address is `/<name>/in` — driving the pipe, which fans out to every
    internal consumer, rather than any one inner port. That is the *direction flip*: a v2
    pipe declares its own type and owns its metadata, so there is no inner target to resolve — the
    describe view alone suffices. (Assumes the described instrument is the top-level graph; nested
    under a host at `/h`, the same pipe is `/h/<name>/in`.)"""
    return f"/{name}/in"


def boundary_controls(boundary: dict) -> list:
    """One fader control per wireable `interface` input pipe, resolved from a `reuben describe
    --json` boundary view. The boundary is already the curated presentational
    metadata, so this path authors nothing — the interface *is* the surface.

    `boundary` is the parsed `describe --json` output, the source of every pipe's
    name/kind/default/min/max/unit/curve. The OSC address derives from the pipe name alone via
    `_osc_from_pipe` (see there for the direction flip).

    Skips inputs a host cannot drive from a fader: a non-fader kind (enum/message/harmony) and a
    range-less numeric (a bare audio pipe, no min/max to scale into)."""
    controls = []
    for port in boundary.get("inputs", []):
        name = port.get("name")
        if not name:
            continue  # a nameless describe entry can't mint an address — skip, don't emit `/None/in`
        if port.get("kind") not in FADER_BOUNDARY_KINDS:
            continue
        if port.get("min") is None or port.get("max") is None:
            continue  # a bare audio pipe (unranged) — not a fader
        lo, hi = float(port["min"]), float(port["max"])
        default = float(port["default"]) if port.get("default") is not None else lo
        # `describe` carries no presentation (label/widget live in a surface
        # doc) — labels here come from the one pinned default_label algorithm.
        controls.append({
            "kind": "fader",
            "label": default_label(str(name)),
            "widget": "fader",
            "address": _osc_from_pipe(name),
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
    """Toggle button -> pipe: the button's `x` (0/1) is the sole arg, set straight onto the
    pipe's `in` port. No scaling — a gate step's domain is already [0,1]."""
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


def _fader_props(name, frame, grid_steps=13) -> str:
    x, y, w, h = frame
    return "".join([
        _pb("background", True), _pb("bar", True), _pi("barDisplay", 0), _pc("color", 1, 0, 0, 1),
        _pf("cornerRadius", 1), _pb("cursor", True), _pi("cursorDisplay", 0), _pr("frame", x, y, w, h),
        _pb("grabFocus", True), _pb("grid", True), _pc("gridColor", 0, 0, 0, 0.25), _pi("gridSteps", grid_steps),
        _pb("interactive", True), _pb("locked", False), _ps("name", name), _pi("orientation", 0),
        _pb("outline", True), _pi("outlineStyle", 1), _pi("pointerPriority", 0), _pi("response", 0),
        _pi("responseFactor", 100), _pi("shape", 1), _pb("visible", True),
    ])


def _radial_props(name, frame, grid_steps=13) -> str:
    x, y, w, h = frame
    # RADIAL = a rotary fader (same single `x` value model as FADER). Property keys +
    # order are cloned from the reference export's RADIAL: it drops the fader's bar/cursor keys and
    # adds `centered`/`inverted`; shape 2 renders the knob. Keep this key set + order in lockstep
    # with fixtures/REUBEN_REF.tosc (FixtureMatchTest asserts it). `gridSteps` defaults to the
    # reference's 13; an integer control overrides it to detent on whole values.
    return "".join([
        _pb("background", True), _pb("centered", False), _pc("color", 1, 0, 0, 1),
        _pf("cornerRadius", 1), _pr("frame", x, y, w, h), _pb("grabFocus", True), _pb("grid", True),
        _pc("gridColor", 0, 0, 0, 0.25), _pi("gridSteps", grid_steps), _pb("interactive", True),
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
      - a run of consecutive `param-toggle` controls sharing one `group` (a sequencer lane —
        each step is its own pipe now, so the lane is identified by the surface doc's shared
        `group` hint) becomes its own row, wrapping at STEP_COLS;
      - a run of consecutive non-toggle controls sharing a `group` hint becomes its own row
        (e.g. a drum channel's knobs), wrapping at STEP_COLS — this is how a caller lays one
        logical group per row regardless of the default column count;
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
            grp, j = c.get("group"), i
            while j < n and controls[j]["kind"] == "param-toggle" and controls[j].get("group") == grp:
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
        return caption, _node(_id(name, c["address"], "widget"), "BUTTON",
                              _button_props(c["label"], wframe),
                              values=_value("x", "0") + _value("touch", "false"),
                              messages=_osc_note(c["address"], c["note"]))
    if c["kind"] == "chord-button":
        caption = f"{c['label']} (deg {int(c['degree'])})"
        return caption, _node(_id(name, c["address"], "widget"), "BUTTON",
                              _button_props(c["label"], wframe),
                              values=_value("x", "0") + _value("touch", "false"),
                              messages=_osc_chord(c["address"], c["degree"]))
    if c["kind"] == "param-toggle":
        return c["label"], _node(_id(name, c["address"], "widget"), "BUTTON",
                                 _button_props(c["label"], wframe),
                                 values=_value("x", _num(c.get("default", 0.0))) + _value("touch", "false"),
                                 messages=_osc_param_toggle(c["address"]))
    # A fader (or radial) needs a numeric range to scale into; a rangeless pipe fell back to
    # the unit range at resolve time only if authored so — guard here with [0, 1] regardless.
    lo, hi = c.get("min", 0.0), c.get("max", 1.0)
    default = c.get("default", lo)
    unit = c.get("unit", "")
    rng = f"{lo:g}–{hi:g}{(' ' + unit) if unit else ''}"
    span = hi - lo
    default_x = 0.0 if span == 0 else max(0.0, min(1.0, (default - lo) / span))
    # `radial` renders the same value as a rotary knob; everything else (default, OSC scaling) is
    # identical to a fader, so only the node type + property block differ.
    # An integer control detents its grid onto whole values; everything else keeps the
    # reference default of 13 grid divisions.
    grid_steps = c.get("steps", 13)
    if c.get("widget") == "radial":
        ctype, props = "RADIAL", _radial_props(c["label"], wframe, grid_steps)
    else:
        ctype, props = "FADER", _fader_props(c["label"], wframe, grid_steps)
    return f"{c['label']} ({rng})", _node(_id(name, c["address"], "widget"), ctype, props,
                                          values=_value("x", _num(default_x)) + _value("touch", "false"),
                                          messages=_osc_fader(c["address"], lo, hi))


def build_tosc(name: str, controls: list, cols: int) -> bytes:
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
            kids.append(_node(_id(name, c["address"], "label"), "LABEL", _label_props(c["label"], lframe), values=lvals))
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

    pe = sub.add_parser("emit", help="project a surface doc (or the derived default) to a .tosc")
    pe.add_argument("instrument")
    pe.add_argument("--surface", default=None,
                    help="explicit surface doc (default: surfaces/<stem>.touchosc.json ?? "
                         "surfaces/<stem>.json ?? auto-derived from the interface pipes)")
    pe.add_argument("--target", default="touchosc", choices=["touchosc"],
                    help="projection target (per-target surface docs resolve as <stem>.<target>.json)")
    pe.add_argument("--host", default="localhost")
    pe.add_argument("--port", type=int, default=9000)
    pe.add_argument("--out", default=None)
    pe.add_argument("--cols", type=int, default=None,
                    help=f"widgets per row (default: the surface doc's `cols`, else {DEFAULT_COLS})")

    pb = sub.add_parser("boundary",
                        help="emit a .tosc from an instrument's `interface` input pipes")
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

    if args.cmd == "boundary":
        # The boundary is served by core `describe --json` — the single source of
        # truth for both the pipe surface *and* the instrument name, so this path reads that view,
        # not the raw instrument document (addresses come from `_osc_from_pipe`).
        if args.describe:
            boundary = _read_json(Path(args.describe))
            # A pre-captured describe view must belong to the positional instrument, else we'd
            # silently surface the wrong patch. Compare declared names (the file stem if unnamed).
            declared = _read_json(inst_path).get("instrument", inst_path.stem)
            described = boundary.get("instrument")
            if described is not None and described != declared:
                sys.exit(f"--describe {args.describe} describes instrument {described!r}, but "
                         f"{inst_path} declares {declared!r} — pass a describe view of the same "
                         f"instrument.")
        else:
            boundary = run_describe(find_reuben(script, args.reuben), inst_path)
        controls = boundary_controls(boundary)
        if not controls:
            sys.exit(f"{inst_path}: no wireable `interface` input pipes to surface — needs an "
                     f"instrument whose `interface.inputs` declares ranged control pipes.")
        out = _surface_out(script, inst_path, args.out)
        out.write_bytes(build_tosc(boundary.get("instrument", inst_path.stem), controls, args.cols))
        print(f"wrote {out} — {len(controls)} control(s) from the interface boundary, "
              f"send to {args.host}:{args.port}")
        print("NOTE: in TouchOSC, set the OSC connection host/port to the machine running reuben.")
        return 0

    # emit: resolve the surface doc (authored ?? derived) against the instrument's pipes.
    instrument = _read_json(inst_path)
    name = instrument.get("instrument", inst_path.stem)
    warnings = []
    if args.surface:
        surface_path = Path(args.surface)
    else:
        surfaces_dir = script.resolve().parents[3] / "surfaces"
        surface_path = find_surface_doc(surfaces_dir, inst_path.stem, args.target)
    if surface_path is not None:
        surface = _read_json(surface_path)
        source = str(surface_path)
    else:
        surface, warnings = derive_surface(instrument, name)
        source = "the derived default surface (no surface doc found)"
    controls, resolve_warnings = resolve_surface(instrument, surface, name)
    for w in warnings + resolve_warnings:
        print(f"warning: {w}", file=sys.stderr)
    if not controls:
        sys.exit(f"{inst_path}: no resolvable surface controls — the surface doc (or the derived "
                 f"default) surfaced nothing this target can render.")
    cols = args.cols if args.cols is not None else surface.get("cols", DEFAULT_COLS)
    out = _surface_out(script, inst_path, args.out)
    out.write_bytes(build_tosc(name, controls, cols))
    print(f"wrote {out} — {len(controls)} control(s) from {source}, send to {args.host}:{args.port}")
    print("NOTE: in TouchOSC, set the OSC connection host/port to the machine running reuben.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

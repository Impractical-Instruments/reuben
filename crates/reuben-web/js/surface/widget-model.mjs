// DOM-free auto-UI inference core for the reuben web player (issue #225).
//
// This is a faithful port of the control-block inference + binding in
// `.claude/skills/control-surface/gen_surface.py` (`load_param_meta`, `_number_form`,
// `node_param`, `is_gate_step`, `resolve_control`, `specs_of`, `collect_controls`,
// `layout_rows`) into a plain ES module with no DOM and no imports beyond node built-ins
// (this file has none). The DOM renderer and the engine driver both consume the pure
// widget model produced here, so the same inference feeds the on-screen surface and the
// headless check.
//
// A reuben instrument's public face is declared per-node as a `control` block (ADR-0018/0022/
// 0028/0032): a spec-object, or an array of them. Each spec resolves to exactly one Widget —
// a fader/radial, a param-toggle (sequencer gate step), a note-toggle (a play button), or a
// chord-button. `emit`/`initial` turn a widget + its raw UI value into the `{address, args}`
// the control channel sends; the address must survive `plan.rs::osc_in_message` ->
// `render.rs::resolve_port`, which is an EXACT port-name match — see the two corrections below.
//
// TWO CORRECTIONS to the reference are baked in (each flagged inline where it lives):
//   #1  isGateStep must accept the `"Gate"` string enum (gate_mode is a string enum now, not
//       an integer), or groovebox's 48 steps wrongly render as degree-range faders.
//   #2  a paramless "Good Button" fader binds to `/<node>/in`, not the bare `/<node>` (a bare
//       address has no port and drops in resolve_port), and rests at the map's `in` value.
//
// The committed oracle (`testdata/expected-widgets.json`) is generated from the reference WITH
// both corrections; `widget-model.test.mjs` deep-equals against it. Numbers flow straight from the
// parsed schema/instrument JSON with no rounding, so the deep-equal is exact.

// --- schema metadata --------------------------------------------------------------------

/**
 * The numeric (fader-settable) form of an `inputs` property's schema, or `null` (ADR-0028).
 *
 * A settable `Float` input is either a plain `{"type":"number", ...}` schema or a `oneOf` of
 * forms, one of which is the `{"type":"number"}` member carrying min/max/default/description.
 * A bare-audio input (only a wire-ref form) and an `Enum` input (a string-enum form) have no
 * number form, so they are not faders and return `null`.
 *
 * @param {object} prop - one `inputs.properties[name]` schema node.
 * @returns {object | null} the number-form schema, or null when the input isn't fader-settable.
 */
function numberForm(prop) {
  if (prop && prop.type === "number") return prop;
  for (const form of prop?.oneOf ?? []) {
    if (form && form.type === "number") return form;
  }
  return null;
}

/**
 * Build `typeName -> { inputName -> {min,max,default,unit,curve} }` for every settable `Float`
 * input, parsed from the schema's per-type `inputs` branches (ADR-0028; each number form's
 * `description` is `"unit: X, curve: Y"`). Non-numeric inputs (audio passthroughs, Enum inputs)
 * carry no number form and are skipped, so they never appear as faders.
 *
 * @param {object} schema - the parsed `instrument.schema.json`.
 * @returns {Object<string, Object<string, {min:number,max:number,default:number,unit:string,curve:string}>>}
 */
export function loadParamMeta(schema) {
  const out = {};
  for (const branch of schema?.$defs?.node?.allOf ?? []) {
    const typeName = branch.if.properties.type.const;
    const props = branch.then?.properties?.inputs?.properties ?? {};
    const params = {};
    for (const [name, prop] of Object.entries(props)) {
      const p = numberForm(prop);
      if (p === null) continue; // audio passthrough or an Enum input — not a fader control
      let unit = "";
      let curve = "Linear";
      for (const raw of String(p.description ?? "").split(",")) {
        const part = raw.trim();
        if (part.startsWith("unit:")) unit = part.slice("unit:".length).trim();
        else if (part.startsWith("curve:")) curve = part.slice("curve:".length).trim();
      }
      params[name] = {
        min: Number(p.minimum ?? 0),
        max: Number(p.maximum ?? 1),
        default: Number(p.default ?? 0),
        unit,
        curve,
      };
    }
    out[typeName] = params;
  }
  return out;
}

// --- control resolution -----------------------------------------------------------------

// The magnitude at/above which a schema range is the "unbounded passthrough" sentinel rather than
// a usable fader range (an `m2s` `in` port is `[-1e6, 1e6]`). See the paramful guard in resolveControl.
const UNBOUNDED = 1e6;

/**
 * A node's literal `inputs[name]` as a finite number, else `fallback` (ADR-0028). A wire-ref
 * (`{from: ...}`), a boolean, or an Enum symbol (`"Gate"`) is not a numeric literal and falls
 * back — this is why gate_mode must be read RAW in isGateStep, not through nodeParam.
 *
 * @param {object} node
 * @param {string} name
 * @param {number} fallback
 * @returns {number}
 */
export function nodeParam(node, name, fallback) {
  const v = node?.inputs?.[name];
  if (typeof v === "boolean" || typeof v !== "number" || !Number.isFinite(v)) return fallback;
  return Number(v);
}

/**
 * True when `param` is a `stepN` on a sequencer running in gate mode — a boolean on/off step
 * (ADR-0022), not a continuous degree. Such a step is faithfully a toggle button: the button's
 * `x` (0/1) is exactly the param's domain.
 *
 * CORRECTION #1 — gate-step detection accepts the `"Gate"` enum symbol.
 *   `gate_mode` is a STRING enum in the current schema (e.g. `"Gate"` / `"Trigger"`), but the
 *   reference tested `node_param(node, "gate_mode", 0) == 1.0`. nodeParam coerces the string to
 *   the numeric fallback (0), so the reference silently returned false for every real gate step
 *   and groovebox's 48 steps rendered as degree-range faders. We read `gate_mode` RAW and accept
 *   the enum symbol `"Gate"` OR the integer `1` (older instruments still encode it numerically).
 *
 * @param {object} node
 * @param {string|undefined} param
 * @returns {boolean}
 */
export function isGateStep(node, param) {
  if (node?.type !== "sequencer" || !param) return false;
  if (!/^step\d+$/.test(param)) return false;
  const gateMode = node?.inputs?.gate_mode; // RAW — do not coerce through nodeParam
  return gateMode === "Gate" || gateMode === 1;
}

/**
 * Resolve one control spec on `node` into the concrete Widget the emitter needs.
 *
 * `spec` is one `control` entry: `{label, param?, unit?, widget?, min?, max?, default?, port?,
 * note?, degree?, group?}`. With no `param`, the value binds to the node's `in` port (a Good
 * Button `map`); with a `param`, it binds to `/<node>/<param>` and pulls range/unit/default from
 * the schema metadata. A `group`, when present, is carried straight through as a layout hint.
 *
 * @param {object} node
 * @param {object} spec
 * @param {object} meta - the map from loadParamMeta.
 * @returns {object} a Widget (kind: fader | param-toggle | note-toggle | chord-button).
 */
export function resolveControl(node, spec, meta) {
  const addr = node.address;
  const typeName = node.type;
  const label = spec.label;
  const widget = spec.widget ?? "fader";
  const param = spec.param;

  let c;

  if (widget === "note-toggle") {
    // A play toggle: fires `<node>/<port> [note, gate]` (e.g. /voicer/notes). The note is a
    // constant, so note-off carries the same MIDI as note-on and matches the held voice.
    const port = spec.port ?? "notes";
    c = {
      kind: "note-toggle",
      label,
      widget,
      address: `${addr}/${port}`,
      note: Number(spec.note ?? 60),
    };
  } else if (widget === "param-toggle" || isGateStep(node, param)) {
    // A boolean step button: sends `<node>/<param> [x]` where `x` is the button's own 0/1.
    // Unlike note-toggle (a constant note to a port, where the value is velocity), the button
    // value IS the payload set onto the param — so on->off carries through. Default = the step's
    // resting param value in the instrument, so the surface mirrors the stored pattern.
    if (param == null) {
      throw new Error(`node ${addr}: param-toggle control ${JSON.stringify(label)} needs a \`param\``);
    }
    c = {
      kind: "param-toggle",
      label,
      widget: "param-toggle",
      address: `${addr}/${param}`,
      node: addr,
      default: Number(spec.default ?? nodeParam(node, param, 0)),
    };
  } else if (widget === "chord-button") {
    // A chord button (V1.3 Chord-player, ADR-0022): a toggle that fires `<node>/<port>
    // [degree, gate]` — a constant scale degree (the chord root) plus the button's gate. Same
    // custom-2-arg payload as note-toggle, but the constant rides as a scale degree (resolved
    // downstream by the `chord` op + voicer), not an absolute MIDI note, so the release carries
    // the same root and stops the held chord.
    const port = spec.port ?? "set";
    c = {
      kind: "chord-button",
      label,
      widget,
      address: `${addr}/${port}`,
      degree: Number(spec.degree ?? 0),
    };
  } else {
    // FADER (or radial — `widget` is carried through verbatim).
    let lo;
    let hi;
    let dflt;
    let unit;
    let address;

    if (param == null) {
      // CORRECTION #2 — a paramless "Good Button" binds to `/<node>/in`, not the bare `/<node>`.
      //   A Good Button is a public `m2s`/`map` whose settable port is `in`. The reference bound
      //   it to the BARE node address (`/<node>`), but `render.rs::resolve_port` matches an exact
      //   port name — a bare address has no port and DROPS silently. So we address `/<node>/in`.
      //   Range comes from the map instance's own `in_min`/`in_max` literals when present (a
      //   ranged `map`), else [0, 1]: nodeParam already returns those very fallbacks, so this is
      //   the reference's own default range. We deliberately do NOT use the schema range for
      //   `in` — m2s's `in` is the unbounded ±1e6 wire sentinel, not a usable fader range; the
      //   engine test + instrument docs use [0, 1]. The rest value is the map's `in` literal
      //   (good-button rests at 0.5), NOT `inputs.default` as the reference read.
      lo = nodeParam(node, "in_min", 0);
      hi = nodeParam(node, "in_max", 1);
      dflt = nodeParam(node, "in", lo);
      unit = spec.unit ?? "";
      address = `${addr}/in`;
    } else {
      // Paramful: range/unit/default come from the param's schema metadata (spec may override).
      const pm = meta?.[typeName]?.[param];
      if (pm == null) {
        throw new Error(
          `node ${addr}: control names param ${JSON.stringify(param)}, which ${JSON.stringify(typeName)} has no metadata for`,
        );
      }
      lo = pm.min;
      hi = pm.max;
      // Seed the rest value from the node's AUTHORED instance literal when it has one, falling
      // back to the schema default — so firing this default on load is a sonic no-op (issue #225:
      // "a default equals the document literal ... so load is sonically a no-op"). The reference
      // read the schema default unconditionally, which is fine for a passive TouchOSC fader but
      // WRONG for the web path that fires initial() on load: it would retune every fader whose
      // instrument authors a non-default value (euclidean's pulses/rotation/decay, djfilter's
      // tempo/resonance) the moment the surface mounts. param-toggles and good-buttons already
      // seed from the instance literal (nodeParam); this aligns paramful faders with them.
      dflt = nodeParam(node, param, pm.default);
      unit = spec.unit ?? pm.unit;
      address = `${addr}/${param}`;
    }

    // Per-spec overrides win over the inferred range/default.
    lo = Number(spec.min ?? lo);
    hi = Number(spec.max ?? hi);

    // Guard the ±1e6 "unbounded" sentinel on the PARAMFUL path too — the same trap CORRECTION #2
    // handles for paramless Good Buttons. An `m2s`'s `in` port has no musical range (its range is
    // whatever it feeds), so its schema range is the ±1e6 passthrough sentinel. A control that
    // spells `param: "in"` with no min/max would inherit that — a silently unusable ±1,000,000
    // fader. When BOTH ends are the sentinel (a genuinely large but one-sided range like m2s
    // `rate` [0, 1e6] is left alone) and the spec supplied no explicit bound, fall back to [0, 1].
    if (Math.abs(lo) >= UNBOUNDED && Math.abs(hi) >= UNBOUNDED) {
      lo = 0;
      hi = 1;
    }

    // Clamp the seeded default into [lo, hi]. Seeding from the authored instance literal can land
    // a default BELOW the fader's own min when an instrument authors an out-of-range value
    // (euclidean authors decay 0.0 while envelope.decay is [0.001, 5]); an unrepresentable default
    // would fire a sub-floor initial() send and mismatch the slider seed. Clamp fixes both.
    dflt = Math.min(hi, Math.max(lo, Number(spec.default ?? dflt)));
    c = { kind: "fader", label, widget, address, min: lo, max: hi, default: dflt, unit };
  }

  // `group` is a layout hint, not a binding: consecutive controls sharing it pack onto one row
  // (see layoutRows). Carried straight through from the spec, present only when declared.
  if (spec.group != null) c.group = spec.group;
  return c;
}

/**
 * A node's `control` value normalised to a list of specs (object or array both accepted).
 *
 * @param {object} node
 * @returns {object[]}
 */
export function specsOf(node) {
  const c = node.control;
  if (c == null) return [];
  return Array.isArray(c) ? c : [c];
}

// --- layout -----------------------------------------------------------------------------

const DEFAULT_COLS = 4;
const STEP_COLS = 16; // a gate-step lane (or a group) lays out as a full-width row, wrapping past this

/**
 * Pack widgets into rows of varying width, preserving declaration order (port of layout_rows):
 *   - a run of consecutive `param-toggle` widgets sharing one `node` (a sequencer lane) becomes
 *     its own row, wrapping at STEP_COLS;
 *   - else a run of consecutive widgets sharing a `group` hint (and NOT param-toggles) becomes
 *     its own row, wrapping at STEP_COLS;
 *   - everything else flows into a uniform grid of `cols`, flushed whenever a run interrupts.
 * So a lane's 16 steps line up as one row, each `group` (e.g. a drum channel's knobs) gets its
 * own row, and the misc controls (tempo, volumes, tone) grid up in between.
 *
 * @param {object[]} widgets
 * @param {number} [cols]
 * @returns {object[][]} rows of widgets.
 */
export function layoutRows(widgets, cols = DEFAULT_COLS) {
  const rows = [];
  let grid = [];

  const flushGrid = () => {
    for (let k = 0; k < grid.length; k += cols) rows.push(grid.slice(k, k + cols));
    grid = [];
  };
  const emitRun = (run) => {
    for (let k = 0; k < run.length; k += STEP_COLS) rows.push(run.slice(k, k + STEP_COLS));
  };

  const n = widgets.length;
  let i = 0;
  while (i < n) {
    const c = widgets[i];
    if (c.kind === "param-toggle") {
      flushGrid();
      const node = c.node;
      let j = i;
      while (j < n && widgets[j].kind === "param-toggle" && widgets[j].node === node) j += 1;
      emitRun(widgets.slice(i, j));
      i = j;
    } else if (c.group != null) {
      flushGrid();
      const grp = c.group;
      let j = i;
      while (j < n && widgets[j].group === grp && widgets[j].kind !== "param-toggle") j += 1;
      emitRun(widgets.slice(i, j));
      i = j;
    } else {
      grid.push(c);
      i += 1;
    }
  }
  flushGrid();
  return rows;
}

/**
 * Build the full surface for an instrument: every node's control specs resolved to widgets, in
 * declaration order, plus the row layout.
 *
 * @param {object} instrument - a parsed instrument JSON.
 * @param {object} paramMeta - the map from loadParamMeta.
 * @returns {{widgets: object[], rows: object[][]}}
 */
export function buildSurface(instrument, paramMeta) {
  const widgets = [];
  for (const node of instrument.nodes ?? []) {
    for (const spec of specsOf(node)) {
      widgets.push(resolveControl(node, spec, paramMeta));
    }
  }
  return { widgets, rows: layoutRows(widgets) };
}

// --- binding ----------------------------------------------------------------------------

// CORRECTION #3 — a chord button's degree must ride the wire as an INTEGER, not a float.
//   The `Note` OSC form (crates/reuben-core/src/vocab/pitch.rs) types an I32 arg as
//   `Pitch::Degree` and an F32 arg as `Pitch::Absolute` (MIDI). `chord.rs` only spells chords
//   for degree pitches — a non-degree note is silently dropped (chord.rs `pitch.degree() =>
//   None => continue`). Bare JS numbers encode as F32 (codec.mjs), so `[degree, gate]` as two
//   floats arrives as Absolute MIDI `degree` and produces NO SOUND. We mark the degree `{i32}`
//   (codec.mjs's explicit integer form) so it lands as `Pitch::Degree`; the gate stays F32
//   velocity. (The reference gen_surface.py has the same latent drift — it emits the degree as a
//   FLOAT partial.) A note-toggle, by contrast, is a real absolute MIDI note and correctly stays
//   F32. Degrees are integer-valued, so `Number.isInteger` in the codec is satisfied.
const chordDegreeArg = (widget) => ({ i32: widget.degree });

/**
 * The load-time default send for a widget, fired once after the engine is `ready` (ADR-0018).
 * Fader/radial default is ALREADY in [min, max], so it is sent RAW (not scaled); toggles rest at
 * gate 0. Args are bare JS numbers (codec.mjs encodes them as F32) except a chord degree, which
 * is `{i32}` (see CORRECTION #3 above).
 *
 * @param {object} widget
 * @returns {{address: string, args: Array<number | {i32: number}>}}
 */
export function initial(widget) {
  switch (widget.kind) {
    case "fader":
      return { address: widget.address, args: [widget.default] };
    case "param-toggle":
      return { address: widget.address, args: [widget.default] };
    case "note-toggle":
      return { address: widget.address, args: [widget.note, 0] };
    case "chord-button":
      return { address: widget.address, args: [chordDegreeArg(widget), 0] };
    default:
      throw new Error(`initial: unknown widget kind ${JSON.stringify(widget.kind)}`);
  }
}

/**
 * The binding fired on a UI interaction: turn a widget + its raw UI value `x` into the control
 * message to send. Used by BOTH render.mjs (on input) and check.mjs (to drive the engine).
 *   - fader / radial:  x in [0,1] is scaled into [min, max].
 *   - param-toggle:    x (0 or 1) is the payload, sent raw.
 *   - note-toggle:     `[note, x]` — constant absolute-MIDI note (F32) + gate x.
 *   - chord-button:    `[{i32: degree}, x]` — constant scale degree (I32, see CORRECTION #3) + gate x.
 * Args are bare JS numbers (codec.mjs encodes them as F32) except a chord degree, which is `{i32}`.
 *
 * @param {object} widget
 * @param {number} x - the widget's raw UI value.
 * @returns {{address: string, args: Array<number | {i32: number}>}}
 */
export function emit(widget, x) {
  switch (widget.kind) {
    case "fader":
      return { address: widget.address, args: [widget.min + x * (widget.max - widget.min)] };
    case "param-toggle":
      return { address: widget.address, args: [x] };
    case "note-toggle":
      return { address: widget.address, args: [widget.note, x] };
    case "chord-button":
      return { address: widget.address, args: [chordDegreeArg(widget), x] };
    default:
      throw new Error(`emit: unknown widget kind ${JSON.stringify(widget.kind)}`);
  }
}

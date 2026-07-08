// DOM renderer + engine binding for the reuben web player's auto-UI (issue #225).
//
// This is the ONLY module in js/surface/ that touches the DOM. The inference core
// (widget-model.mjs) is pure and DOM-free; here we turn its widget model into on-screen
// controls and wire each control's events back through `emit`/`initial` to
// `engine.send()`. The renderer is a THIN shim: it never re-derives an address, a
// range, or a scaling — every message it sends comes verbatim from `emit(widget, x)`
// or `initial(widget)`, so the on-screen surface and the headless check (check.mjs)
// drive the engine through the exact same binding.
//
// IMPORT-SAFE UNDER `node --test`: this file imports at module scope but never touches
// `document`/`window` there — every DOM access lives inside a function body and reads
// the `document` global lazily. So render.test.mjs can `import` it, install a minimal
// fake `document` on globalThis, and exercise renderSurface with no browser and no wasm.
//
// LIFECYCLE (why sendInitialDefaults is separate from renderSurface): a control message
// sent to the worklet BEFORE the instrument is constructed is dropped on the floor — the
// worklet has no engine to route it to until `load()` resolves (its `ready` reply, see
// reuben-engine.mjs). renderSurface only BUILDS the DOM and wires listeners; it sends
// nothing. The caller renders as soon as it likes, but must call sendInitialDefaults
// ONLY AFTER `await engine.load(name)` resolves, so the load-time defaults actually land.
// A user interaction can't beat that either — the surface isn't on screen until render,
// and render is invoked post-ready in main.js.

import { emit, initial } from "./widget-model.mjs";

// --- styles ------------------------------------------------------------------------------

// A uniform, reflowing CSS grid (the ADR-0018 idiom — relative units, never absolute px):
// each row is a grid whose columns auto-fit to the viewport, so the same surface reads on
// phone / tablet / desktop. A 16-button step lane keeps 16 columns and scrolls horizontally
// on a narrow screen rather than squashing; a grouped channel row wraps the same way.
export const SURFACE_CSS = `
.reuben-surface { display: flex; flex-direction: column; gap: 0.75rem; margin: 1rem 0; }
.reuben-surface:empty::after {
  content: "Self-playing instrument — no player controls.";
  color: #888; font-size: 0.9rem; font-style: italic;
}
.surface-row {
  display: grid; gap: 0.5rem;
  grid-template-columns: repeat(auto-fit, minmax(7rem, 1fr));
}
.surface-row.step-lane {
  grid-auto-flow: column; grid-auto-columns: minmax(1.6rem, 1fr);
  overflow-x: auto; gap: 0.25rem;
}
.surface-widget {
  display: flex; flex-direction: column; gap: 0.25rem; align-items: stretch;
  padding: 0.4rem 0.5rem; border: 1px solid #ccc; border-radius: 0.5rem; min-width: 0;
}
.surface-widget .surface-label { font-size: 0.8rem; font-weight: 600; }
.surface-widget .surface-value {
  font-size: 0.75rem; color: #555; font-variant-numeric: tabular-nums;
}
.surface-widget.fader input, .surface-widget.radial input { width: 100%; }
/* radials read as knobs: a compact, centered slider distinct from the wide fader. */
.surface-widget.radial { align-items: center; }
.surface-widget.radial input { max-width: 5rem; }
button.surface-widget {
  cursor: pointer; font: inherit; text-align: center; background: #f4f4f4;
}
button.surface-widget.on { background: #2a7ad0; color: #fff; border-color: #1c5aa0; }
button.surface-widget.step-cell { padding: 0.5rem 0.2rem; font-size: 0.7rem; }
`;

// Inject SURFACE_CSS once into <head>, guarded so the DOM-free test stub (no head /
// no getElementById) simply skips it — styling is inessential to the binding under test.
function ensureStyles(doc) {
  if (!doc || typeof doc.getElementById !== "function" || !doc.head) return;
  if (doc.getElementById("reuben-surface-styles")) return;
  const style = doc.createElement("style");
  style.id = "reuben-surface-styles";
  style.textContent = SURFACE_CSS;
  doc.head.appendChild(style);
}

// --- value formatting --------------------------------------------------------------------

// Human-legible fader read-out: round to 2 dp and drop trailing zeros. Scaling itself is
// NOT done here — the number shown is exactly what emit() put on the wire.
function fmtValue(v) {
  if (!Number.isFinite(v)) return String(v);
  return String(Math.round(v * 100) / 100);
}

// --- widget builders ---------------------------------------------------------------------

// A fader or radial: a range input over the normalised [0,1] domain that emit() scales into
// [min,max]. The slider position is the widget's raw `x`; on every `input` we route x through
// emit(widget, x) — the renderer never touches min/max/address itself.
function buildFader(doc, widget, engine) {
  const el = doc.createElement("div");
  el.classList.add("surface-widget", widget.widget); // "fader" or "radial"

  const label = doc.createElement("span");
  label.classList.add("surface-label");
  label.textContent = widget.label;

  const value = doc.createElement("span");
  value.classList.add("surface-value");

  const input = doc.createElement("input");
  input.setAttribute("type", "range");
  input.setAttribute("min", "0");
  input.setAttribute("max", "1");
  input.setAttribute("step", "0.001");

  // The widget's default is ALREADY in [min,max]; the slider works in [0,1], so seed its
  // position by inverting the scale. (emit/initial do the forward scale — we only invert
  // here to place the thumb, never to send.)
  const span = widget.max - widget.min;
  const norm = span === 0 ? 0 : (widget.default - widget.min) / span;
  input.value = String(norm);

  const unit = widget.unit ? ` ${widget.unit}` : "";
  value.textContent = `${fmtValue(widget.default)}${unit}`;

  input.addEventListener("input", () => {
    const x = Number(input.value);
    const { address, args } = emit(widget, x);
    engine.send(address, args);
    value.textContent = `${fmtValue(args[0])}${unit}`;
  });

  el.appendChild(label);
  el.appendChild(input);
  el.appendChild(value);
  return el;
}

// A latching toggle (param-toggle / sequencer gate step): the button's pressed state IS the
// payload. Click flips 0<->1 and sends emit(widget, x); we seed the pressed state from the
// widget's stored default so the surface mirrors the instrument's saved pattern.
function buildParamToggle(doc, widget, engine) {
  const el = doc.createElement("button");
  el.setAttribute("type", "button");
  el.classList.add("surface-widget", "param-toggle", "step-cell");
  el.textContent = widget.label;

  let on = Number(widget.default) === 1;
  const reflect = () => {
    el.setAttribute("aria-pressed", on ? "true" : "false");
    if (on) el.classList.add("on");
    else el.classList.remove("on");
  };
  reflect();

  el.addEventListener("click", () => {
    on = !on;
    reflect();
    const { address, args } = emit(widget, on ? 1 : 0);
    engine.send(address, args);
  });
  return el;
}

// A momentary hold button (note-toggle / chord-button): press = gate on, release = gate off,
// so it plays only while held (hold-to-play). pointerup / pointerleave / pointercancel ALL
// release, so a pointer that slides off the button still stops the held voice (no stuck note).
function buildHoldButton(doc, widget, engine) {
  const el = doc.createElement("button");
  el.setAttribute("type", "button");
  el.classList.add("surface-widget", widget.kind); // "note-toggle" or "chord-button"
  el.textContent = widget.label;

  const gate = (x) => {
    const { address, args } = emit(widget, x);
    engine.send(address, args);
  };
  el.addEventListener("pointerdown", () => {
    el.classList.add("on");
    gate(1);
  });
  // Release on up / leave / cancel. A redundant note-off (leave with no prior down) is
  // harmless — it carries the same constant note/degree and simply re-stops a stopped voice.
  const release = () => {
    el.classList.remove("on");
    gate(0);
  };
  el.addEventListener("pointerup", release);
  el.addEventListener("pointerleave", release);
  el.addEventListener("pointercancel", release);
  return el;
}

function buildWidget(doc, widget, engine) {
  switch (widget.kind) {
    case "fader":
      return buildFader(doc, widget, engine); // covers widget.widget "fader" and "radial"
    case "param-toggle":
      return buildParamToggle(doc, widget, engine);
    case "note-toggle":
    case "chord-button":
      return buildHoldButton(doc, widget, engine);
    default:
      throw new Error(`renderSurface: unknown widget kind ${JSON.stringify(widget.kind)}`);
  }
}

// --- public API --------------------------------------------------------------------------

// Row modifier class, mirroring layoutRows' three run kinds (see widget-model.mjs): a param-toggle
// run is a step lane (16 cols, scrolls); a grouped run is a channel row; everything else is
// the uniform auto-fit grid. Pure styling — layout truth already lives in surface.rows.
function rowClass(row) {
  const first = row[0];
  if (!first) return null;
  if (first.kind === "param-toggle") return "step-lane";
  if (first.group != null) return "group-row";
  return null;
}

/**
 * Render `surface.rows` into `container`, wiring every widget's events through emit() to
 * `engine.send`. Clears `container` first (safe to call again on a toy switch). Sends
 * NOTHING — see the lifecycle note atop this file; the caller fires sendInitialDefaults
 * after the engine is ready. An instrument with no controls yields an empty (but present)
 * surface — no crash, the ":empty" CSS labels it self-playing.
 *
 * @param {{widgets: object[], rows: object[][]}} surface - from buildSurface().
 * @param {{send: (address: string, args: number[]) => void}} engine
 * @param {object} container - a DOM element to render into.
 */
export function renderSurface(surface, engine, container) {
  const doc = globalThis.document;
  ensureStyles(doc);

  container.textContent = ""; // clear any prior surface before re-rendering
  container.classList?.add("reuben-surface");

  for (const row of surface.rows ?? []) {
    const rowEl = doc.createElement("div");
    rowEl.classList.add("surface-row");
    const cls = rowClass(row);
    if (cls) rowEl.classList.add(cls);
    for (const widget of row) {
      rowEl.appendChild(buildWidget(doc, widget, engine));
    }
    container.appendChild(rowEl);
  }
}

/**
 * Fire each widget's load-time default exactly once. MUST be called AFTER engine.load()
 * resolves (post-`ready`): a control sent before construct is dropped (see the lifecycle
 * note atop this file). Routes through initial() — the renderer derives no addresses.
 *
 * @param {{widgets: object[]}} surface - from buildSurface().
 * @param {{send: (address: string, args: number[]) => void}} engine
 */
export function sendInitialDefaults(surface, engine) {
  for (const widget of surface.widgets ?? []) {
    const { address, args } = initial(widget);
    engine.send(address, args);
  }
}

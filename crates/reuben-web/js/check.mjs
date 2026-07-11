#!/usr/bin/env node
// CI gate for the reuben web player (#224): drives the real release wasm through the
// documented C-ABI lifecycle (crates/reuben-web/src/bridge.rs) exactly the way the worklet
// will — one persistent instance, destroy() between instruments — and machine-checks:
//
//   1. the module instantiates outside a browser (imports = {env: {log}} only)
//   2. registry_count() > 0 AND equals the operator count pinned by the committed schema
//      (crates/reuben-core/schema/instrument.schema.json) — the silent codegen-unit
//      ctor-drop failure class (ADR-0040 §4)
//   3. the full instrument matrix loads via the fetch-on-miss loop and renders ~2 s
//      (750 quanta) of finite audio; self-playing instruments must be non-silent;
//      mic-space exercises the duplex input path with deterministic synthetic input
//   4. the control channel changes the audio: groovebox at default tempo vs 180 BPM
//   5. bad paths fail loudly (render before construct, construct with no document,
//      malformed control buffer, garbage WAV bytes, malformed document JSON)
//
// Usage: node check.mjs   (from any cwd — paths resolve via import.meta.url)
// Prerequisite: cargo build --release --target wasm32-unknown-unknown  (in crates/reuben-web)

import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

import { encodeControl } from "./codec.mjs";
import { writeBytes, readError, readReport, loadInstrument } from "./loader.mjs";
import { wasmIntrospect } from "./tools.mjs";
import { buildSurface, emit } from "./surface/widget-model.mjs";

const WASM_URL = new URL(
  "../target/wasm32-unknown-unknown/release/reuben_web.wasm",
  import.meta.url,
);
const SCHEMA_URL = new URL(
  "../../reuben-core/schema/instrument.schema.json",
  import.meta.url,
);
const INSTRUMENTS_URL = new URL("../../../instruments/", import.meta.url);
const SURFACES_URL = new URL("../../../surfaces/", import.meta.url);

// The committed schema, parsed ONCE, feeds the registry-count pin ONLY — the surface path is
// schema-free under ADR-0043 (the instrument's `interface` block carries the whole contract).
// A parse failure here aborts the run loudly — the schema is committed and must be valid.
const schema = JSON.parse(await readFile(SCHEMA_URL, "utf8"));

// Two captured planar streams are identical iff same length and every sample equal.
function streamsDiffer(a, b) {
  if (a.length !== b.length) return true;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return true;
  return false;
}

const SAMPLE_RATE = 48000;
const BLOCK = 128; // asserted against block_size() below — JS never trusts its own copy
const QUANTA = 750; // ~2 s at 128 frames / 48 kHz — enough for every clock to fire
const SILENCE_RMS = 1e-4;

// The instrument matrix (#224): the whole curated library — the five bundled Toys plus the
// default rig. SELF_PLAYING must render non-silent audio unprompted; NOTE_DRIVEN only has to
// load and render finitely (nothing plays a note in CI); DUPLEX gets deterministic synthetic
// input and must pass it through audibly.
const SELF_PLAYING = [
  "groovebox",
  "euclidean-drums",
];
const NOTE_DRIVEN = [
  "default",
  "chord-player",
  "strum-harp",
];
const DUPLEX = ["mic-space"];

const decoder = new TextDecoder();
const encoder = new TextEncoder();
const failures = [];
let passes = 0;

function check(ok, label) {
  console.log(`${ok ? "PASS" : "FAIL"}  ${label}`);
  if (ok) passes += 1;
  else failures.push(label);
}

// --- instantiate the one persistent instance (the worklet's real lifecycle) -------------

let wasmBytes;
try {
  wasmBytes = await readFile(WASM_URL);
} catch {
  console.error(
    `FAIL  wasm artifact missing: ${fileURLToPath(WASM_URL)}\n` +
      "      run `cargo build --release --target wasm32-unknown-unknown` in crates/reuben-web first",
  );
  process.exit(1);
}

let mem = null;
const { instance } = await WebAssembly.instantiate(wasmBytes, {
  env: {
    log: (ptr, len) =>
      console.log(
        `  [wasm] ${mem ? decoder.decode(new Uint8Array(mem.buffer, ptr, len)) : `<log before memory export: ptr=${ptr} len=${len}>`}`,
      ),
  },
});
const ex = instance.exports;
mem = ex.memory;
// Same ctor dance as the worklet: on this toolchain lld usually synthesizes ctor calls
// into the exports, but run whichever init export exists; registry_count() proves the
// outcome either way.
if (typeof ex._initialize === "function") ex._initialize();
else if (typeof ex.__wasm_call_ctors === "function") ex.__wasm_call_ctors();
check(true, "module instantiates with imports {env: {log}}");

// --- helpers (memory-view discipline: re-wrap every view, every call) -------------------

async function fetchResource(key) {
  // Disk stand-in for `fetch(assetBase + key)`: canonical keys are root-relative
  // (e.g. "voices/kick-voice.json", "patches/space.json").
  const buf = await readFile(new URL(key, INSTRUMENTS_URL));
  return new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength);
}

function queueControlBytes(bytes) {
  const ptr = writeBytes(ex, bytes);
  const rc = ex.queue_control(ptr, bytes.length);
  ex.dealloc(ptr, bytes.length);
  return rc;
}

/// Render `quanta` quanta on the live engine; returns stats and (optionally) the full
/// planar sample stream. `writeInput(input, q, inCh)` stages synthetic input -> render(1).
function renderQuanta(quanta, { writeInput = null, capture = false } = {}) {
  const ch = ex.channels();
  const stream = capture ? new Float32Array(quanta * ch * BLOCK) : null;
  let peak = 0;
  let sumSq = 0;
  let bad = 0;
  for (let q = 0; q < quanta; q++) {
    let hasInput = 0;
    if (writeInput) {
      const inCh = ex.input_channels();
      // Re-wrap the input view every quantum — growth detaches old views.
      const input = new Float32Array(mem.buffer, ex.input_ptr(), inCh * BLOCK);
      writeInput(input, q, inCh);
      hasInput = 1;
    }
    if (ex.render(hasInput) !== 0) throw new Error(`render(${hasInput}) failed at quantum ${q}`);
    // Re-wrap the output view every quantum too.
    const out = new Float32Array(mem.buffer, ex.output_ptr(), ch * BLOCK);
    for (const s of out) {
      if (!Number.isFinite(s)) bad += 1;
      const a = Math.abs(s);
      if (a > peak) peak = a;
      sumSq += s * s;
    }
    if (stream) stream.set(out, q * ch * BLOCK);
  }
  return { peak, rms: Math.sqrt(sumSq / (quanta * ch * BLOCK)), bad, stream };
}

/// Deterministic synthetic mic: 330 Hz sine at 0.25 amplitude, phase-continuous across
/// quanta, written planar (input[ch*BLOCK + f]) like the worklet does.
function syntheticInput(input, q, inCh) {
  for (let c = 0; c < inCh; c++) {
    for (let f = 0; f < BLOCK; f++) {
      const t = q * BLOCK + f;
      input[c * BLOCK + f] = 0.25 * Math.sin((2 * Math.PI * 330 * t) / SAMPLE_RATE);
    }
  }
}

async function loadByName(name) {
  const doc = await readFile(new URL(`${name}.json`, INSTRUMENTS_URL), "utf8");
  return loadInstrument(ex, doc, SAMPLE_RATE, fetchResource);
}

// --- bad paths that must come before any construct ---------------------------------------

console.log("\n=== bad paths (pre-construct) ===");
check(ex.render(0) !== 0, "render(0) before any construct returns nonzero");
{
  const rc = ex.construct(SAMPLE_RATE);
  const err = readError(ex);
  check(
    rc === 1 && err.length > 0,
    `construct with no document returns 1 with error: "${err}"`,
  );
}

// --- registry pin (ADR-0040 §4: the silent ctor-drop failure class) ----------------------

console.log("\n=== registry pin ===");
{
  // The committed schema is generated from Registry::builtin(): $defs.node has one
  // `type` enum variant (and one allOf if/then arm) per registered operator. Counting
  // the enum pins registry_count() to the committed operator set.
  let expected = null;
  let source = "schema $defs.node.properties.type.enum";
  const variants = schema?.$defs?.node?.properties?.type?.enum;
  if (Array.isArray(variants) && variants.length > 0) expected = variants.length;
  if (expected === null) {
    // LOUD FALLBACK: the schema's shape changed and the enum count above came up empty.
    // 53 tracks Registry::builtin() as of 2026-07 — if this fires, fix the schema walk
    // (and bump this number only when operators are genuinely added/removed).
    expected = 53;
    source = "hardcoded 53 (schema walk failed — tracks Registry::builtin())";
  }
  const count = ex.registry_count();
  check(
    count > 0 && count === expected,
    `registry_count() = ${count}, expected ${expected} from ${source}`,
  );

  // The format_version() export (issue #228) — the share-link boot path distinguishes an
  // envelope/document from a newer engine by it. Tracks reuben_core::format::FORMAT_VERSION;
  // bump the 3 only when the format version genuinely bumps (same convention as the count
  // above). v3 = ADR-0043: presentation stripped out of the instrument document.
  const fv = ex.format_version();
  check(fv === 3, `format_version() = ${fv}, expected 3 (reuben_core::format::FORMAT_VERSION)`);
}

// --- authoring exports (introspection) ---------------------------------------------------
//
// ADR-0052 §2: the three introspection contracts as new wasm exports over
// reuben_core::introspect — the in-page tool layer's anchors, reusing the exact OS-free
// contract types the native lane serializes (§5: one schema, two doors). Stateless over the
// module — no live engine needed. describe_* return 0 = ok (read the report) / 1 = error (read
// the error); validate returns 0 whenever a report was produced (a {ok:false} report is a
// SUCCESSFUL call, ADR-0048 §3), 1 only on bad UTF-8.

console.log("\n=== authoring exports (introspection) ===");
{
  // Write `text` into linear memory, call the export, free the buffer, return the rc.
  const callDoc = (fn, text) => {
    const bytes = encoder.encode(text);
    const ptr = writeBytes(ex, bytes);
    const rc = fn(ptr, bytes.length);
    ex.dealloc(ptr, bytes.length);
    return rc;
  };
  const parseReport = () => JSON.parse(readReport(ex));

  // A minimal self-contained instrument (no external resources): oscillator -> output.
  const GOOD_DOC = JSON.stringify({
    format_version: 3,
    instrument: "probe",
    interface: { outputs: { out: { from: "/out.audio" } } },
    nodes: [
      { type: "oscillator", address: "/osc", inputs: { freq: 220.0 } },
      { type: "output", address: "/out", inputs: { audio: { from: "/osc" } } },
    ],
  });
  // Same shape with an unknown operator type at /osc (typo) — validation fails, localized.
  const BROKEN_DOC = JSON.stringify({
    format_version: 3,
    instrument: "probe",
    nodes: [{ type: "oscilllator", address: "/osc" }],
    outputs: [],
  });

  // describe_operators(all): name_len 0 lists the whole registry.
  {
    const rc = ex.describe_operators(0, 0);
    let ops = null;
    try {
      ops = parseReport().operators;
    } catch {}
    const names = Array.isArray(ops) ? ops.map((o) => o.type_name) : [];
    check(
      rc === 0 &&
        Array.isArray(ops) &&
        ops.length === ex.registry_count() &&
        ["oscillator", "filter", "voicer"].every((n) => names.includes(n)),
      `describe_operators(all) rc=${rc}: ${names.length} ops == registry_count ${ex.registry_count()}, includes osc/filter/voicer`,
    );
  }

  // describe_operators("oscillator"): exactly one op, with a freq input.
  {
    const rc = callDoc(ex.describe_operators, "oscillator");
    let ops = null;
    try {
      ops = parseReport().operators;
    } catch {}
    const one = Array.isArray(ops) && ops.length === 1 ? ops[0] : null;
    check(
      rc === 0 &&
        one &&
        one.type_name === "oscillator" &&
        one.inputs.some((p) => p.name === "freq"),
      `describe_operators("oscillator") rc=${rc}: one op, type_name oscillator, has freq input`,
    );
  }

  // describe_operators("nope"): isError, and the error names the missing type.
  {
    const rc = callDoc(ex.describe_operators, "nope");
    const err = readError(ex);
    check(
      rc === 1 && err.includes("nope"),
      `describe_operators("nope") rc=${rc} (isError), error names it: "${err}"`,
    );
  }

  // describe_instrument(good doc): PatchBoundary names the document's instrument.
  {
    const rc = callDoc(ex.describe_instrument, GOOD_DOC);
    let boundary = null;
    try {
      boundary = parseReport();
    } catch {}
    check(
      rc === 0 && boundary && boundary.instrument === "probe",
      `describe_instrument(good) rc=${rc}: boundary.instrument === "probe"`,
    );
  }

  // describe_instrument(bad json): isError, error non-empty.
  {
    const rc = callDoc(ex.describe_instrument, "{ not json");
    const err = readError(ex);
    check(
      rc === 1 && err.length > 0,
      `describe_instrument("{ not json") rc=${rc} (isError): "${err}"`,
    );
  }

  // validate(good doc): rc 0, a clean report.
  {
    const rc = callDoc(ex.validate, GOOD_DOC);
    let report = null;
    try {
      report = parseReport();
    } catch {}
    check(
      rc === 0 &&
        report &&
        report.ok === true &&
        Array.isArray(report.errors) &&
        report.errors.length === 0 &&
        Array.isArray(report.warnings) &&
        report.warnings.length === 0,
      `validate(good) rc=${rc}: {ok:true, errors:[], warnings:[]}`,
    );
  }

  // validate(broken doc): rc 0 (NOT a throw/1 — a failed validation is a successful call,
  // ADR-0048 §3), report ok:false with a diag localizing the offending node.
  {
    const rc = callDoc(ex.validate, BROKEN_DOC);
    let report = null;
    try {
      report = parseReport();
    } catch {}
    check(
      rc === 0 &&
        report &&
        report.ok === false &&
        Array.isArray(report.errors) &&
        report.errors.length >= 1 &&
        report.errors[0].node === "/osc",
      `validate(broken) rc=${rc}: {ok:false}, errors[0].node === "/osc" (${report ? report.errors.length : 0} error(s))`,
    );
  }

  // Leave a clean slate for the instrument matrix (no staged doc/resources leaked).
  ex.destroy();
}

// --- content_hash + the wasmIntrospect adapter (issue #353) -------------------------------
//
// ADR-0052 §3/§5: the fourth authoring export (content_hash) plus the JS adapter
// (tools.mjs wasmIntrospect) the in-page tool layer binds. content_hash mints an opaque,
// stable token over a document's canonical bytes — byte-identical to native's (§5). This drives
// describe_operators / describe_instrument / validate THROUGH the adapter too, proving
// wasmIntrospect wraps the real exports. The engine-bound tools (send/swap/…) need a real
// AudioWorklet, so they stay covered by tools.test.mjs with fakes — no worklet is spun here.

console.log("\n=== content_hash + wasmIntrospect adapter ===");
{
  const introspect = wasmIntrospect(ex);

  // A minimal self-contained instrument (no external resources): oscillator -> output.
  const GOOD_DOC = JSON.stringify({
    format_version: 3,
    instrument: "probe",
    interface: { outputs: { out: { from: "/out.audio" } } },
    nodes: [
      { type: "oscillator", address: "/osc", inputs: { freq: 220.0 } },
      { type: "output", address: "/out", inputs: { audio: { from: "/osc" } } },
    ],
  });
  // Same document with one changed node (the oscillator's freq).
  const CHANGED_DOC = GOOD_DOC.replace("220", "440");

  // content_hash: a non-empty token, stable across an equal doc, different on a changed node.
  {
    const h1 = introspect.contentHash(GOOD_DOC);
    const h2 = introspect.contentHash(GOOD_DOC);
    const h3 = introspect.contentHash(CHANGED_DOC);
    check(
      typeof h1 === "string" && h1.length > 0 && h1 === h2 && h1 !== h3,
      `contentHash "${h1}": non-empty, stable (==), differs on a changed node (!= "${h3}")`,
    );
  }

  // describe_operators through the adapter: the whole registry, and one named op with a freq input.
  {
    const all = introspect.describeOperators();
    const osc = introspect.describeOperators("oscillator");
    check(
      Array.isArray(all) &&
        all.length === ex.registry_count() &&
        Array.isArray(osc) &&
        osc.length === 1 &&
        osc[0].type_name === "oscillator" &&
        osc[0].inputs.some((p) => p.name === "freq"),
      `wasmIntrospect.describeOperators: ${all.length} ops == registry_count, "oscillator" has a freq input`,
    );
  }

  // describe_operators("nope") through the adapter THROWS (the isError mapping).
  {
    let threw = false;
    try {
      introspect.describeOperators("nope");
    } catch {
      threw = true;
    }
    check(threw, `wasmIntrospect.describeOperators("nope") throws (isError)`);
  }

  // describe_instrument through the adapter: the PatchBoundary names the document's instrument.
  {
    const boundary = introspect.describeInstrument(GOOD_DOC);
    check(
      boundary && boundary.instrument === "probe",
      `wasmIntrospect.describeInstrument(good): boundary.instrument === "probe"`,
    );
  }

  // validate through the adapter: a good doc reports ok:true (and does NOT throw).
  {
    const report = introspect.validate(GOOD_DOC);
    check(
      report && report.ok === true && Array.isArray(report.errors) && report.errors.length === 0,
      `wasmIntrospect.validate(good): {ok:true, errors:[]}`,
    );
  }

  // validate through the adapter: a BROKEN doc returns {ok:false} with a localized Diag[] and
  // does NOT throw — the load-bearing "a failed validation is a successful call" (ADR-0048 §3),
  // driven end-to-end through the REAL adapter (not just the fake in tools.test.mjs).
  {
    const BROKEN_DOC = JSON.stringify({
      format_version: 3,
      instrument: "probe",
      nodes: [{ type: "oscilllator", address: "/osc" }],
      outputs: [],
    });
    let report = null;
    let threw = false;
    try {
      report = introspect.validate(BROKEN_DOC);
    } catch {
      threw = true;
    }
    check(
      !threw &&
        report &&
        report.ok === false &&
        Array.isArray(report.errors) &&
        report.errors.length >= 1 &&
        report.errors[0].node === "/osc",
      `wasmIntrospect.validate(broken): {ok:false}, errors[0].node "/osc", NO throw (ADR-0048 §3)`,
    );
  }

  // describe_instrument through the adapter: a doc that fails to load THROWS (the isError mapping).
  {
    let threw = false;
    try {
      introspect.describeInstrument("{ not json");
    } catch {
      threw = true;
    }
    check(threw, `wasmIntrospect.describeInstrument("{ not json") throws (isError)`);
  }

  // Leave a clean slate for the instrument matrix (no staged doc/resources leaked).
  ex.destroy();
}

// --- the instrument matrix on one persistent instance -------------------------------------

console.log("\n=== instrument matrix ===");
const matrix = [
  ...SELF_PLAYING.map((n) => [n, "SELF_PLAYING"]),
  ...NOTE_DRIVEN.map((n) => [n, "NOTE_DRIVEN"]),
  ...DUPLEX.map((n) => [n, "DUPLEX"]),
];
for (const [name, cls] of matrix) {
  try {
    const info = await loadByName(name);
    if (info.blockSize !== BLOCK) {
      throw new Error(`block_size() = ${info.blockSize}, expected ${BLOCK}`);
    }
    if (info.channels < 1) throw new Error("channels() = 0 after a ready construct");
    if (cls === "DUPLEX" && info.inputChannels !== 1) {
      throw new Error(`input_channels() = ${info.inputChannels}, expected 1`);
    }
    const stats = renderQuanta(QUANTA, {
      writeInput: cls === "DUPLEX" ? syntheticInput : null,
    });
    if (stats.bad > 0) throw new Error(`${stats.bad} non-finite sample(s)`);
    if (cls !== "NOTE_DRIVEN" && stats.rms <= SILENCE_RMS) {
      throw new Error(`silent output (rms ${stats.rms.toExponential(2)})`);
    }
    check(
      true,
      `${name} [${cls}]: ${QUANTA} quanta finite, rms ${stats.rms.toFixed(5)}, peak ${stats.peak.toFixed(3)}`,
    );
  } catch (e) {
    check(false, `${name} [${cls}]: ${e.message}`);
  } finally {
    // Toy switch on the SAME instance — the worklet's real lifecycle.
    ex.destroy();
  }
}

// --- control channel: the /tempo pipe must change groovebox's audio -----------------------
//
// ADR-0043: groovebox's tempo is an interface input pipe; players set /tempo/in (the clock's
// own tempo port is wire-fed FROM the pipe, so the pipe address is the one that routes).

console.log("\n=== control channel (groovebox /tempo/in) ===");
try {
  await loadByName("groovebox");
  const base = renderQuanta(QUANTA, { capture: true });
  ex.destroy();

  await loadByName("groovebox");
  const rc = queueControlBytes(encodeControl("/tempo/in", [180]));
  check(rc === 0, "queue_control(/tempo/in [180]) returns 0");
  const fast = renderQuanta(QUANTA, { capture: true });

  check(
    base.bad === 0 && fast.bad === 0,
    `both streams finite (bad: ${base.bad} + ${fast.bad})`,
  );
  check(
    base.rms > SILENCE_RMS && fast.rms > SILENCE_RMS,
    `both streams non-silent (rms ${base.rms.toFixed(5)} vs ${fast.rms.toFixed(5)})`,
  );
  check(
    streamsDiffer(base.stream, fast.stream),
    "tempo 180 stream differs from default-tempo stream",
  );
} catch (e) {
  check(false, `control check: ${e.message}`);
} finally {
  ex.destroy();
}

// --- auto-UI generated-widget binding (#225 / #247) ---------------------------------------
//
// The pure model tests (surface/widget-model.test.mjs) prove buildSurface produces the right widget
// SHAPES, but a pure test can't prove the emitted (address, args) actually ROUTE through the
// live engine. This section is the guard that closes that gap: for each of the 6 surfaces it
// builds the widgets from the SAME instrument + surface-doc JSON the browser uses (ADR-0043 —
// pipes carry the contract, no schema involved), picks ONE representative resolved widget,
// drives it via emit() -> encodeControl() -> queue_control, and asserts the resolved address
// changed the audio — i.e. `/<pipe>/in` survived plan.rs::osc_in_message ->
// render.rs::resolve_port (an exact port-name match). Every emitted address is a pipe's `in`
// port now, so the picks instead cover every PAYLOAD shape a bundled Toy exposes: a scaled
// fader value, a 0/1 param-toggle, a chord-button [{i32 degree}, gate], and a dragged fader
// gesture. (The absolute-note toggle payload lost its host when good-button left the library
// in the cull — no surviving Toy declares an absolute-note pipe.) The engine driving reuses the tempo-check lifecycle: render a base
// capture on a fresh construct, destroy, construct again, queue the widget's message, render
// a driven capture — then assert both finite, the driven stream non-silent, and the two
// streams differ.

console.log("\n=== auto-UI generated-widget binding ===");

// The surface is resolved from the parsed instrument + surface-doc JSON, exactly as the
// worklet's UI does — no engine needed to BUILD it (only to prove it routes).
async function surfaceOf(name) {
  const doc = JSON.parse(await readFile(new URL(`${name}.json`, INSTRUMENTS_URL), "utf8"));
  const surfaceDoc = JSON.parse(await readFile(new URL(`${name}.json`, SURFACES_URL), "utf8"));
  return buildSurface(doc, surfaceDoc);
}

function widgetAt(widgets, address) {
  const w = widgets.find((x) => x.address === address);
  if (!w) throw new Error(`no generated widget addresses ${address}`);
  return w;
}

// Render the untouched base stream for `name` on a fresh construct, then destroy (the toy-switch
// lifecycle). The driven stream is captured by the caller after a second construct.
async function baseStream(name) {
  await loadByName(name);
  const s = renderQuanta(QUANTA, { capture: true });
  ex.destroy();
  return s;
}

// Both streams finite, the driven stream non-silent (note/chord-driven instruments have a silent
// base, so the assertion is on the AFTER stream), and the two differ — proving the address routed.
function assertBinding(name, address, base, driven) {
  check(
    base.bad === 0 && driven.bad === 0,
    `${name}: ${address} both streams finite (bad ${base.bad} + ${driven.bad})`,
  );
  check(
    driven.rms > SILENCE_RMS,
    `${name}: ${address} driven stream non-silent (rms ${driven.rms.toFixed(5)})`,
  );
  check(
    streamsDiffer(base.stream, driven.stream),
    `${name}: ${address} (generated widget) changes audio`,
  );
}

// Drive one generated widget with a single emitted message and assert it changed the audio.
// The wire args come straight from emit() — no re-typing — so this exercises the exact path the
// on-screen surface takes (including CORRECTION #3: a chord degree rides as {i32}).
async function bindingCheck(name, address, x) {
  const { widgets } = await surfaceOf(name);
  const widget = widgetAt(widgets, address);
  const msg = emit(widget, x);
  const base = await baseStream(name);
  await loadByName(name);
  const rc = queueControlBytes(encodeControl(msg.address, msg.args));
  check(rc === 0, `${name}: queue_control(${msg.address}) returns 0`);
  const driven = renderQuanta(QUANTA, { capture: true });
  ex.destroy();
  assertBinding(name, msg.address, base, driven);
}

try {
  // groovebox — a PARAM-TOGGLE step gate (the kick_step2 pipe). The step rests off; emit(w,1)
  // sets it on, adding a kick hit to the self-playing pattern. Proves a param-toggle's 0/1
  // payload routes through a pipe's `in` port.
  await bindingCheck("groovebox", "/kick_step2/in", 1);

  // euclidean-drums — a RADIAL on the kick_filter pipe (bipolar [-1,1], from the PIPE's own
  // declared range). emit(w,1.0) sweeps the kick's per-channel djfilter to full high-pass,
  // thinning the kit. Proves a radial (kind fader) routes on a self-playing kit.
  await bindingCheck("euclidean-drums", "/kick_filter/in", 1.0);

  // chord-player — a CHORD-BUTTON (the `chord` note pipe) tapping the I chord. NOTE-DRIVEN: base
  // silent, driven non-silent. A scale DEGREE must ride as an I32: boundary.rs's Note OscForm
  // reads an integer arg as Pitch::Degree and a float as Pitch::Absolute, and chord.rs drops
  // non-degree notes (a held chord button only sounds when its root is a Degree). emit() types
  // the degree as {i32}, so driving the widget straight through emit() sounds the chord — this
  // line is the guard that the typed payload routes.
  await bindingCheck("chord-player", "/chord/in", 1);

  // strum-harp — the Strum bar FADER (the strum pipe). Strumming is a GESTURE: the strum op
  // plucks a string each time the position crosses a band BETWEEN rendered blocks, so a single
  // jump plucks nothing (verified) — it must be dragged. We emit()-drive a drag of positions
  // 1/16..16/16 with a render between each (exactly as dragging the on-screen fader does), then
  // capture the ringing aftermath. Proves /strum/in routes to audio.
  {
    const name = "strum-harp";
    const { widgets } = await surfaceOf(name);
    const widget = widgetAt(widgets, "/strum/in");
    const base = await baseStream(name);
    await loadByName(name);
    const DRAG = 16;
    let dragRc = 0;
    for (let k = 1; k <= DRAG; k++) {
      const msg = emit(widget, k / DRAG);
      dragRc |= queueControlBytes(encodeControl(msg.address, msg.args));
      renderQuanta(8); // advance time so the smoothed position crosses string bands and plucks
    }
    check(dragRc === 0, `${name}: queue_control(/strum/in drag) all return 0`);
    const driven = renderQuanta(QUANTA, { capture: true });
    ex.destroy();
    assertBinding(name, "/strum/in", base, driven);
  }
} catch (e) {
  check(false, `auto-UI binding: ${e.message}`);
} finally {
  ex.destroy();
}

// --- remaining bad paths -------------------------------------------------------------------

console.log("\n=== bad paths (post-construct) ===");
try {
  await loadInstrument(ex, "{ not json", SAMPLE_RATE, fetchResource);
  check(false, "malformed document text was accepted");
} catch (e) {
  check(
    e.message.length > 0,
    `malformed document fails construct with error: "${e.message}"`,
  );
}
ex.destroy();

{
  // Garbage bytes staged as kind 1 (WAV) must be rejected at stage time.
  const key = encoder.encode("samples/garbage.wav");
  const data = new Uint8Array([0xde, 0xad, 0xbe, 0xef, 1, 2, 3]);
  const keyPtr = writeBytes(ex, key);
  const dataPtr = writeBytes(ex, data);
  const rc = ex.stage_resource(keyPtr, key.length, 1, dataPtr, data.length);
  ex.dealloc(keyPtr, key.length);
  ex.dealloc(dataPtr, data.length);
  check(rc === 1, `stage_resource(kind 1, garbage bytes) returns 1: "${readError(ex)}"`);
}
ex.destroy();

try {
  await loadByName("default");
  check(
    queueControlBytes(new Uint8Array([1, 2, 3])) === 1,
    "malformed control buffer [1,2,3] returns 1 after a successful construct",
  );
} catch (e) {
  check(false, `malformed-control setup: ${e.message}`);
} finally {
  ex.destroy();
}

// --- summary --------------------------------------------------------------------------------

console.log("");
if (failures.length === 0) {
  console.log(`${passes} checks passed`);
  process.exit(0);
}
console.log(`${failures.length} of ${passes + failures.length} check(s) FAILED:`);
for (const f of failures) console.log(`  - ${f}`);
process.exit(1);

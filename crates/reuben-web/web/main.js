// Throwaway demo/harness (issue #224) — main-thread half of the demo page.
//
// Drives js/reuben-engine.mjs (which codes against crates/reuben-web/src/bridge.rs and
// src/codec.rs). Everything heavy is pre-staged at page load; the Start button's
// handler does ONLY ctx.resume() — the smallest reliable iOS audio unlock (P1, #223).
//
// Serving requires staging (see the comment atop index.html for the full recipe):
//   cd crates/reuben-web
//   cargo build --release --target wasm32-unknown-unknown
//   cp target/wasm32-unknown-unknown/release/reuben_web.wasm js/
//   ln -s ../../../instruments web/instruments
//   python3 -m http.server -d .        ->  http://localhost:8000/web/

import { createReubenEngine } from "../js/reuben-engine.mjs";

const el = (id) => document.getElementById(id);
const startBtn = el("start");
const loadBtn = el("load");
const micBtn = el("mic");
const noteBtn = el("note");
const instrumentSel = el("instrument");
const tempoSlider = el("tempo");
const tempoValue = el("tempo-value");
const statusEl = el("status");
const logEl = el("log");

let engine = null;

function setStatus(text, cls) {
  statusEl.textContent = text;
  statusEl.className = cls || "";
}

function appendLog(text) {
  console.log(text);
  logEl.textContent += `${text}\n`;
  logEl.scrollTop = logEl.scrollHeight;
}

async function prestage() {
  engine = await createReubenEngine({ assetBase: "./instruments" });
  engine.onLog = appendLog;
  // Enable Start NOW, not on any worklet reply: a suspended context may not run the
  // worklet's message handler until resumed (browser-dependent), so gating the button
  // on readiness can deadlock (P1 finding). Start only resumes the pre-staged context.
  startBtn.disabled = false;
  loadBtn.disabled = false;
  setStatus("Pre-staged — Load an instrument, press Start to unmute");
}

startBtn.addEventListener("click", async () => {
  // ONLY resume — everything else happened at page load. Errors go on-page: iOS has
  // no devtools, and resume() can reject (NotAllowedError / interrupted context).
  try {
    await engine.context.resume();
    setStatus(`Running (context: ${engine.context.state})`, "ok");
    appendLog(`[page] context resumed, state=${engine.context.state}`);
  } catch (err) {
    setStatus(`FAILED — ctx.resume(): ${err}`, "bad");
    appendLog(`[page] ctx.resume() rejected: ${err}`);
  }
});

loadBtn.addEventListener("click", async () => {
  const name = instrumentSel.value;
  loadBtn.disabled = true;
  setStatus(`Loading ${name}…`);
  try {
    const info = await engine.load(name);
    micBtn.disabled = false;
    noteBtn.disabled = false;
    setStatus(
      `${name} ready — ${info.channels} ch out, ${info.inputChannels} ch in, ` +
        `${info.blockSize}-frame blocks` +
        (engine.context.state === "running" ? "" : " (press Start to unmute)"),
      "ok",
    );
    appendLog(`[page] loaded ${name}: ${JSON.stringify(info)}`);
  } catch (err) {
    setStatus(`FAILED — load ${name}: ${err.message || err}`, "bad");
    appendLog(`[page] load failed: ${err.stack || err}`);
  } finally {
    loadBtn.disabled = false;
  }
});

micBtn.addEventListener("click", async () => {
  try {
    await engine.enableMic();
    setStatus("Mic connected", "ok");
    appendLog("[page] mic connected to worklet input");
  } catch (err) {
    // enableMic throws friendly messages (denied / no device) — show them, don't die.
    setStatus(err.message, "bad");
    appendLog(`[page] mic: ${err.message}`);
  }
});

// Tempo slider -> /clock/tempo (bare number encodes as F32). Live on "input" so
// dragging sweeps the clock on groovebox / metronome / euclidean-drums etc.
tempoSlider.addEventListener("input", () => {
  const bpm = Number(tempoSlider.value);
  tempoValue.textContent = String(bpm);
  if (engine) engine.send("/clock/tempo", [bpm]);
});

// Note button: A440 on, then off 300 ms later — /voicer/notes [note, gate].
noteBtn.addEventListener("click", () => {
  engine.send("/voicer/notes", [69, 1]);
  setTimeout(() => engine.send("/voicer/notes", [69, 0]), 300);
});

prestage().catch((err) => {
  setStatus(`FAILED — pre-stage: ${err}`, "bad");
  appendLog(`[page] ${err.stack || err}`);
});

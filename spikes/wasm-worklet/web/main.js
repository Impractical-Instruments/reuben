// Throwaway spike (issue #223) — main-thread half.
//
// Everything is pre-staged at page load (context, worklet module, WASM compile, node
// construction, connection); the Start button's handler does ONLY ctx.resume() — the
// smallest, most reliable iOS audio-unlock action, guaranteeing instant sound.

const WASM_URL = "./reuben_wasm_worklet_spike.wasm";
// ?instrument=sequence exercises the resolver + voicer-host stretch; default is the
// vibrato gate (0). Keeps the page single-button per the spike design.
const INSTRUMENT =
  new URLSearchParams(location.search).get("instrument") === "sequence" ? 1 : 0;

const startBtn = document.getElementById("start");
const statusEl = document.getElementById("status");
const logEl = document.getElementById("log");
const decoder = new TextDecoder();

let ctx = null;
let ready = false;
let wasmModule = null; // main-thread compile, used only by the headroom bench instance

function setStatus(text, cls) {
  statusEl.textContent = text;
  statusEl.className = cls || "";
}

function appendLog(text) {
  console.error(text);
  const line = document.createElement("div");
  line.textContent = text;
  logEl.appendChild(line);
}

function onWorkletMessage(e) {
  const msg = e.data;
  if (msg.type === "log") {
    appendLog(`[wasm] ${msg.bytes ? decoder.decode(msg.bytes) : msg.text}`);
  } else if (msg.type === "error") {
    const reason = msg.bytes ? `: ${decoder.decode(msg.bytes)}` : "";
    setStatus(`FAILED — ${msg.text}${reason}`, "bad");
    appendLog(`[worklet] ${msg.text}${reason}`);
  } else if (msg.type === "blocks") {
    if (ready && ctx.state === "running") {
      setStatus(`Playing — ${msg.count} blocks rendered`, "ok");
    }
  } else if (msg.type === "ready") {
    ready = true;
    appendLog(
      `[worklet] ready at ${msg.sampleRate} Hz, registry: ${msg.registryCount} operators`,
    );
    setStatus(
      ctx.state === "running"
        ? "Playing"
        : "Ready — press Start (iOS: mute switch OFF, volume up)",
      "ok",
    );
    measureHeadroom().catch((err) => appendLog(`[bench] failed: ${err}`));
  }
}

// Checkpoint 5's number: batch-timed mean render cost vs the quantum budget, measured on
// a second, unconnected instance of the same module (batch timing sidesteps sub-ms timer
// coarsening at 128 frames; main thread is a fine proxy for a spike).
async function measureHeadroom() {
  const BLOCKS = 2000;
  let mem = null;
  const imports = {
    env: {
      log: (ptr, len) =>
        appendLog(`[bench wasm] ${decoder.decode(new Uint8Array(mem.buffer, ptr, len))}`),
    },
  };
  const inst = await WebAssembly.instantiate(wasmModule, imports);
  const ex = inst.exports;
  mem = ex.memory;
  if (typeof ex._initialize === "function") ex._initialize();
  else if (typeof ex.__wasm_call_ctors === "function") ex.__wasm_call_ctors();
  if (ex.init(ctx.sampleRate, INSTRUMENT) !== 0) throw new Error("bench init failed");
  // Warm-up (first blocks hit lazy one-time work), then the timed batch.
  for (let i = 0; i < 100; i++) ex.render();
  const t0 = performance.now();
  for (let i = 0; i < BLOCKS; i++) ex.render();
  const perBlockMs = (performance.now() - t0) / BLOCKS;
  const budgetMs = (128 / ctx.sampleRate) * 1000;
  appendLog(
    `[bench] mean render ${perBlockMs.toFixed(4)} ms/block vs ${budgetMs.toFixed(3)} ms ` +
      `budget -> ${(budgetMs / perBlockMs).toFixed(0)}x headroom (main-thread proxy)`,
  );
}

async function prestage() {
  setStatus("Pre-staging…");
  ctx = new (window.AudioContext || window.webkitAudioContext)();
  await ctx.audioWorklet.addModule("./worklet.js");
  const resp = await fetch(WASM_URL);
  if (!resp.ok) throw new Error(`fetch ${WASM_URL}: HTTP ${resp.status}`);
  const bytes = await resp.arrayBuffer();
  // compile (not compileStreaming): identical everywhere, incl. older Safari. This
  // module stays main-thread — it feeds the headroom bench, NOT the worklet.
  wasmModule = await WebAssembly.compile(bytes);
  const node = new AudioWorkletNode(ctx, "reuben-spike", {
    numberOfInputs: 0,
    numberOfOutputs: 1,
    outputChannelCount: [2],
  });
  node.port.onmessage = onWorkletMessage;
  node.connect(ctx.destination);
  // Send raw BYTES, not the compiled module: Chromium silently fails to deliver a
  // structured-cloned WebAssembly.Module to a worklet (messageerror, no diagnostics).
  // The worklet sync-compiles them itself — it can't fetch. Transfer, don't copy:
  // `wasmModule` above already owns its own copy of the code.
  node.port.postMessage({ type: "module", bytes, instrument: INSTRUMENT }, [bytes]);
  // Enable Start NOW, not on the worklet's ready message: a suspended context may not
  // run the worklet's message handler until resumed (browser-dependent), so gating the
  // button on `ready` can deadlock. Start only resumes the pre-staged context; worklet
  // init completes within milliseconds either side of it.
  startBtn.disabled = false;
  setStatus("Waiting for worklet init… press Start");
}

startBtn.addEventListener("click", async () => {
  // ONLY resume — everything else already happened at page load (see header comment).
  try {
    await ctx.resume();
  } catch (err) {
    // resume() can reject (NotAllowedError/InvalidStateError, e.g. an interrupted iOS
    // context); without this the tap does nothing and NO diagnostic reaches the page.
    setStatus(`FAILED — ctx.resume(): ${err}`, "bad");
    appendLog(`[page] ctx.resume() rejected: ${err}`);
    return;
  }
  if (ready) setStatus("Playing", "ok");
  appendLog(`[page] context resumed, state=${ctx.state}`);
});

document.getElementById("which").textContent =
  INSTRUMENT === 1 ? "sequence (stretch)" : "vibrato (gate)";

prestage().catch((err) => {
  setStatus(`FAILED — pre-stage: ${err}`, "bad");
  appendLog(`[page] ${err.stack || err}`);
});

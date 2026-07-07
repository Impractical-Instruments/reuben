// reuben-engine.mjs — the main-thread ES-module API of the reuben web player
// (issue #224). This is the module a page imports.
//
// Codes against `crates/reuben-web/src/bridge.rs` (the flat C-ABI over WebShell) via
// `js/loader.mjs`, and against `crates/reuben-web/src/codec.rs` via `js/codec.mjs`.
//
// Two instances of the same WASM module play two roles (bridge.rs's design):
// - the DISCOVERY instance, here on the main thread, runs the fetch-on-miss loop to
//   learn an instrument's complete resource bundle (it can await fetches; the worklet
//   cannot), and is destroyed-but-kept between loads;
// - the PERSISTENT instance inside the AudioWorkletProcessor (js/worklet.js) receives
//   the complete pre-discovered bundle and renders.
//
// Text never crosses the worklet boundary as strings the worklet must encode/decode:
// AudioWorkletGlobalScope guarantees neither TextEncoder nor TextDecoder, so this side
// pre-encodes the document and resource keys to UTF-8 bytes and decodes the worklet's
// log/error bytes.
//
// Start-gesture handling (context.resume()) is deliberately the PAGE's job — this
// module never auto-resumes. The P1 pattern (#223): enable Start at pre-stage;
// resume() alone is the smallest reliable iOS audio unlock.

import { encodeControl } from "./codec.mjs";
import { loadInstrument } from "./loader.mjs";

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/**
 * Create a reuben engine: worklet module + node wired to the destination, WASM fetched
 * and posted in, main-thread discovery instance ready. Returns immediately-usable API;
 * the page still owns the start gesture (`engine.context.resume()`).
 *
 * @param {object} options
 * @param {string} options.assetBase - base URL for instrument JSON and resources
 *   (document at `${assetBase}/${name}.json`, resources at `${assetBase}/${key}`)
 * @param {string|URL} [options.wasmUrl] - default: reuben_web.wasm next to this module
 * @param {string|URL} [options.workletUrl] - default: worklet.js next to this module
 * @param {AudioContext} [options.context] - reuse a context instead of creating one
 */
export async function createReubenEngine({
  assetBase,
  wasmUrl = new URL("./reuben_web.wasm", import.meta.url),
  workletUrl = new URL("./worklet.js", import.meta.url),
  context,
} = {}) {
  if (!assetBase) throw new Error("createReubenEngine: assetBase is required");

  const ownsContext = !context;
  const ctx = context ?? new (globalThis.AudioContext || globalThis.webkitAudioContext)();

  await ctx.audioWorklet.addModule(workletUrl);

  // Fetch the WASM bytes ONCE. Compile the main-thread (discovery) module from them
  // first — compile doesn't detach — then transfer the raw bytes to the worklet, which
  // sync-compiles its own copy (Chromium silently drops structured-cloned Modules to
  // worklets; P1 finding).
  const resp = await fetch(wasmUrl);
  if (!resp.ok) throw new Error(`fetch ${wasmUrl}: HTTP ${resp.status}`);
  const wasmBytes = await resp.arrayBuffer();
  const wasmModule = await WebAssembly.compile(wasmBytes);

  const node = new AudioWorkletNode(ctx, "reuben-engine", {
    numberOfInputs: 1,
    numberOfOutputs: 1,
    outputChannelCount: [2],
  });

  // --- Worklet message plumbing: log decode + pending-reply resolvers. ---

  const pending = new Map(); // reply type ("ready" | "destroyed") -> {resolve, reject}

  function expectReply(type) {
    if (pending.has(type)) {
      // Thrown (not a rejected promise) so the caller's postMessage never happens.
      throw new Error(`a "${type}" operation is already in flight`);
    }
    return new Promise((resolve, reject) => pending.set(type, { resolve, reject }));
  }

  function settle(type, err, value) {
    const p = pending.get(type);
    if (!p) return false;
    pending.delete(type);
    if (err) p.reject(err);
    else p.resolve(value);
    return true;
  }

  function log(text) {
    engine.onLog(text);
  }

  node.port.onmessage = (e) => {
    const msg = e.data;
    switch (msg.type) {
      case "log":
        log(`[wasm] ${msg.bytes ? decoder.decode(msg.bytes) : msg.text}`);
        break;
      case "ready":
        settle("ready", null, {
          channels: msg.channels,
          inputChannels: msg.inputChannels,
          blockSize: msg.blockSize,
        });
        break;
      case "destroyed":
        settle("destroyed", null, undefined);
        break;
      case "error": {
        const detail = msg.bytes ? `: ${decoder.decode(msg.bytes)}` : "";
        const err = new Error(`${msg.text}${detail}`);
        // An error during a load fails that load; otherwise it just gets surfaced.
        if (!settle("ready", err)) log(`[worklet error] ${err.message}`);
        break;
      }
      default:
        log(`[worklet] unknown message type "${msg.type}"`);
    }
  };
  node.port.onmessageerror = () =>
    log("[worklet] port messageerror on the main thread: reply could not be deserialized");
  node.onprocessorerror = (e) => log(`[worklet] processor error: ${e}`);

  node.connect(ctx.destination);
  // Transfer, don't copy: the compiled wasmModule above already owns its own copy.
  node.port.postMessage({ type: "module", bytes: wasmBytes }, [wasmBytes]);

  // --- The reusable main-thread discovery instance (lazy, kept across loads). ---

  let discovery = null; // exports, once instantiated
  async function discoveryExports() {
    if (discovery) return discovery;
    const box = { ex: null };
    const imports = {
      env: {
        log: (ptr, len) => {
          // Guard: `log` can fire before instantiate returns (a panic in a start
          // function) — a TypeError here would mask the real diagnostic.
          if (!box.ex?.memory) {
            log("[discovery] <log call before memory was available>");
            return;
          }
          log(`[discovery] ${decoder.decode(new Uint8Array(box.ex.memory.buffer, ptr, len))}`);
        },
      },
    };
    const instance = await WebAssembly.instantiate(wasmModule, imports);
    const ex = instance.exports;
    box.ex = ex;
    // Ctor dance (toolchain-portable; current LLD may synthesize ctors into every
    // export, in which case neither hook exists).
    if (typeof ex._initialize === "function") ex._initialize();
    else if (typeof ex.__wasm_call_ctors === "function") ex.__wasm_call_ctors();
    discovery = ex;
    return ex;
  }

  let micStream = null;
  let micSource = null;
  let micPending = null; // in-flight getUserMedia, so a double click can't leak a stream
  let loading = false; // one load() at a time: the discovery instance is shared state
  let destroyed = false;

  const engine = {
    context: ctx,
    node,

    /** Override to route diagnostics somewhere visible (defaults to console.log). */
    onLog: (text) => console.log(text),

    /**
     * Load `${assetBase}/${name}.json`: run fetch-on-miss discovery on the main
     * thread, collect the complete bundle, ship doc + bundle to the worklet, await
     * its construct. Resolves {channels, inputChannels, blockSize}.
     */
    async load(name) {
      // One at a time, guarded BEFORE the discovery instance is touched: two
      // overlapping loads would satisfy each other's misses on the shared instance
      // and each ship an incomplete bundle.
      if (loading) throw new Error("a load is already in flight");
      if (destroyed) throw new Error("engine was destroyed");
      loading = true;
      try {
        const docUrl = `${assetBase}/${name}.json`;
        const docResp = await fetch(docUrl);
        if (!docResp.ok) throw new Error(`fetch ${docUrl}: HTTP ${docResp.status}`);
        const docText = await docResp.text();

        // Discovery pass: loadInstrument stages each fetched resource into the
        // discovery instance; the fetchResource callback also collects the bytes into
        // the bundle map, so what reaches the worklet is exactly what construct needed.
        const ex = await discoveryExports();
        const bundle = new Map(); // key -> {kind, bytes}
        try {
          await loadInstrument(ex, docText, ctx.sampleRate, async (key, kind) => {
            const url = `${assetBase}/${key}`;
            const r = await fetch(url);
            if (!r.ok) throw new Error(`fetch ${url}: HTTP ${r.status}`);
            const bytes = new Uint8Array(await r.arrayBuffer());
            bundle.set(key, { kind, bytes });
            return bytes;
          });
        } finally {
          // Keep the instance for reuse; drop its engine + staged bundle.
          ex.destroy();
        }

        // Ship the complete set. Keys and document are pre-encoded to UTF-8 bytes (the
        // worklet has no TextEncoder); buffers are transferred, not copied.
        const doc = encoder.encode(docText);
        const entries = [...bundle].map(([key, { kind, bytes }]) => ({
          key, // kept as a string for the worklet's error messages
          keyBytes: encoder.encode(key),
          kind,
          bytes,
        }));
        const transfers = [doc.buffer];
        for (const e of entries) transfers.push(e.keyBytes.buffer, e.bytes.buffer);
        const reply = expectReply("ready");
        node.port.postMessage({ type: "load", doc, bundle: entries }, transfers);
        return await reply;
      } finally {
        loading = false;
      }
    },

    /** Encode one control message (codec.mjs) and post it to the worklet. */
    send(address, args = []) {
      const buffer = encodeControl(address, args);
      node.port.postMessage({ type: "control", buffer }, [buffer.buffer]);
    },

    /** Ask for the microphone and wire it into the worklet node's input. */
    async enableMic() {
      if (micSource) return; // already live
      if (micPending) return micPending; // a request is mid-flight; don't leak a 2nd stream
      if (!globalThis.navigator?.mediaDevices?.getUserMedia) {
        throw new Error("Microphone unavailable: getUserMedia is not supported here");
      }
      micPending = (async () => {
        let stream;
        try {
          stream = await navigator.mediaDevices.getUserMedia({ audio: true });
        } catch (err) {
          if (err && (err.name === "NotAllowedError" || err.name === "SecurityError")) {
            throw new Error("Microphone permission denied — allow mic access and try again");
          }
          if (err && (err.name === "NotFoundError" || err.name === "OverconstrainedError")) {
            throw new Error("No microphone found on this device");
          }
          throw new Error(`Microphone failed: ${err}`);
        }
        if (destroyed) {
          // destroy() ran while we awaited the permission prompt; don't wire a corpse.
          for (const track of stream.getTracks()) track.stop();
          return;
        }
        micStream = stream;
        micSource = ctx.createMediaStreamSource(stream);
        micSource.connect(node);
      })();
      try {
        return await micPending;
      } finally {
        micPending = null;
      }
    },

    /**
     * Tear down: destroy the worklet's engine, disconnect nodes, stop mic tracks.
     * Closes the AudioContext only if this module created it. Idempotent.
     */
    destroy() {
      if (destroyed) return;
      destroyed = true;
      // A pending load can never be answered once the node is torn down.
      settle("ready", new Error("engine destroyed"));
      node.port.postMessage({ type: "destroy" });
      if (micSource) {
        micSource.disconnect();
        micSource = null;
      }
      if (micStream) {
        for (const track of micStream.getTracks()) track.stop();
        micStream = null;
      }
      node.disconnect();
      if (ownsContext) ctx.close().catch(() => {});
    },
  };

  return engine;
}

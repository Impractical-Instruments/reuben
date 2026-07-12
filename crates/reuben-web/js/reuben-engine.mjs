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
import { raisedCosineDuckCurve, DECLICK_EDGE_MS } from "./declick.mjs";

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

  // A master GainNode between the worklet and the speakers — M1-web's ONLY master-gain stage, and
  // so the only place the re-strike's declicked duck (spec §6.2.4, issue #360) can ride. ADR-0050
  // §2 puts that raised-cosine ramp on the CORE RT-side install slot "inherited by both shells",
  // but §1 ships it with M2's mailbox swap ("M1 stays rude — no M1 work item") and the core master
  // path has no gain stage yet (§Context). M1's swap is stop-the-world (ADR-0046 §10 / ADR-0052 §2:
  // destroy → stage → construct in the worklet), so the restart edges live in THIS shell — and this
  // node is where `restrikeDuck` fades to silence and back around them. Steady-state gain is unity;
  // it only moves during a duck (see js/declick.mjs for the WHERE-the-declick-lives finding).
  const masterGain = ctx.createGain();
  node.connect(masterGain);
  masterGain.connect(ctx.destination);
  // Transfer, don't copy: the compiled wasmModule above already owns its own copy.
  node.port.postMessage({ type: "module", bytes: wasmBytes }, [wasmBytes]);

  // --- Main-thread instances of the shared module. ---

  // Instantiate a new instance with the {env:{log}} import and run the ctor dance. `logPrefix`
  // tags this instance's diagnostics ("discovery" | "fragment"); the caller owns caching.
  async function instantiateExports(logPrefix) {
    const box = { ex: null };
    const imports = {
      env: {
        log: (ptr, len) => {
          // Guard: `log` can fire before instantiate returns (a panic in a start
          // function) — a TypeError here would mask the real diagnostic.
          if (!box.ex?.memory) {
            log(`[${logPrefix}] <log call before memory was available>`);
            return;
          }
          log(`[${logPrefix}] ${decoder.decode(new Uint8Array(box.ex.memory.buffer, ptr, len))}`);
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
    return ex;
  }

  // The reusable main-thread discovery instance (lazy, kept across loads).
  let discovery = null;
  async function discoveryExports() {
    if (!discovery) discovery = await instantiateExports("discovery");
    return discovery;
  }

  // A FRESH, uncached instance — the fragment-boot path (loadBundle) instantiates one per boot
  // and discards it, rather than reusing `discovery`. Two reasons (issue #228 boot path): a Rust
  // abort traps the instance and leaves the `static mut` shell in an arbitrary state the next
  // load() would inherit; and wasm linear memory never shrinks, so an envelope that balloons
  // memory before failing would leave the tab holding it for the session if it poisoned the
  // reusable instance.
  function freshExports() {
    return instantiateExports("fragment");
  }

  // Ship a fully-discovered bundle to the worklet and await its construct. Keys + document are
  // pre-encoded to UTF-8 bytes (the worklet has no TextEncoder); every buffer is transferred,
  // not copied. Shared by load() (fetch-backed discovery) and loadBundle() (bundle-backed).
  async function shipBundle(docText, bundle) {
    // Retain a copy for Share (issue #228): the transfer below DETACHES the originals, so snapshot
    // the document + resource bytes now, before they leave. Bundles are small (< 25 KB for the
    // largest rig), so a copy per load is cheap and makes Share work uniformly for a Toy opened
    // from the launcher and one booted from a link.
    lastLoaded = {
      docText,
      resources: [...bundle].map(([key, { kind, bytes }]) => ({ key, kind, bytes: bytes.slice() })),
    };
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
  }

  let micStream = null;
  let micSource = null;
  let micPending = null; // in-flight getUserMedia, so a double click can't leak a stream
  let loading = false; // one load() at a time: the discovery instance is shared state
  let destroyed = false;
  let lastLoaded = null; // {docText, resources:[{key,kind,bytes}]} of the last load, for Share

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

        // Ship the complete set to the worklet and await construct.
        return await shipBundle(docText, bundle);
      } finally {
        loading = false;
      }
    },

    /**
     * Boot from a pre-resolved bundle (issue #228 fragment boot): a document plus every resource
     * it references, decoded from a share link. Discovery runs on a FRESH instance (see
     * freshExports) that is discarded when this returns — success or failure. The bundle-backed
     * fetchResource NEVER falls back to the network: a miss is a hard failure (ADR-0042, decision
     * 1) surfaced as an error with `code: "incomplete"` (the caller maps it to failure class I).
     * A structural/version/JSON failure surfaces the engine's verbatim message (classes F/G/H).
     * Resolves the same {channels, inputChannels, blockSize} as load().
     *
     * @param {object} args
     * @param {string} args.docText - the top-level document, verbatim from the link.
     * @param {Map<string,{kind:number,bytes:Uint8Array}>|Array<{key:string,kind:number,bytes:Uint8Array}>} [args.resources]
     */
    async loadBundle({ docText, resources }) {
      if (loading) throw new Error("a load is already in flight");
      if (destroyed) throw new Error("engine was destroyed");
      loading = true;
      try {
        const supplied =
          resources instanceof Map
            ? resources
            : new Map((resources ?? []).map((r) => [r.key, { kind: r.kind, bytes: r.bytes }]));

        // Fresh instance, discarded on failure (never the reused discovery instance).
        const ex = await freshExports();
        const bundle = new Map();
        try {
          await loadInstrument(ex, docText, ctx.sampleRate, async (key) => {
            const hit = supplied.get(key);
            if (!hit) {
              // A bundle miss is terminal — an origin fetch would make a broken link appear to
              // work on the one host that serves the missing file and 404 everywhere else.
              throw Object.assign(new Error(`bundle is missing resource: ${key}`), {
                code: "incomplete",
              });
            }
            bundle.set(key, { kind: hit.kind, bytes: hit.bytes });
            return hit.bytes;
          });
        } finally {
          ex.destroy(); // drop the fresh instance's engine + staged bundle; the instance is discarded
        }

        return await shipBundle(docText, bundle);
      } finally {
        loading = false;
      }
    },

    /**
     * The bundle most recently loaded — `{docText, resources: [{key, kind, bytes}]}` — or null
     * before the first load. The document + resource bytes are retained copies (the originals
     * were transferred to the worklet), so Share (issue #228) can re-encode them into a link
     * without re-running discovery, whether the instrument came from a Toy card or a fragment.
     */
    currentBundle() {
      return lastLoaded;
    },

    /** Encode one control message (codec.mjs) and post it to the worklet. */
    send(address, args = []) {
      const buffer = encodeControl(address, args);
      node.port.postMessage({ type: "control", buffer }, [buffer.buffer]);
    },

    /**
     * Post a PRE-ENCODED control buffer verbatim — a snapshot entry replayed from a share link
     * (issue #228). It is already an encodeControl() buffer, so this skips re-encoding and puts
     * the exact bytes the worklet consumes on the wire. Copied before transfer so the caller's
     * snapshot array isn't detached.
     */
    sendRaw(buffer) {
      const bytes = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer);
      const copy = bytes.slice();
      node.port.postMessage({ type: "control", buffer: copy }, [copy.buffer]);
    },

    /**
     * The re-strike's declicked duck (spec §6.2.4, issue #360): fade the master output to silence,
     * run `atSilence` at the trough, then fade back up — a raised-cosine ramp per edge, NEVER a hard
     * cut. This is where the structural restart's audio edge is declicked in M1-web (see the
     * WHERE-it-lives finding in js/declick.mjs; the engine-side core ramp of ADR-0050 §2 is M2).
     *
     * `atSilence` is the co-timed cause (spec §6.2.1): the caller commits the change-card, animates
     * the surface, and resets the playhead HERE, so cause and effect are simultaneous — the change
     * lands exactly as the sound drops. In the agent-wired flow it is also where the real restart-swap
     * (engine.loadBundle, the worklet reconstruct) belongs, so the reconstruct happens under silence.
     *
     * Instant re-strike for v1 (spec §6.3): the trough is the ~12ms edge boundary, not a held gap —
     * quantize-to-downbeat (holding the swap for a bar line) is a named deferred door, not built here.
     * Resolves when the up-edge completes. If the engine is torn down (no context/gain), `atSilence`
     * still runs (so the visible commit never stalls) and it resolves immediately — a silent no-op duck.
     *
     * @param {() => (void | Promise<void>)} atSilence - the trough work (commit + reconstruct), run
     *   once the output has reached silence.
     * @param {{edgeMs?: number}} [opts] - edge duration per side (default DECLICK_EDGE_MS; ADR-0050
     *   §3's fixed 5–20ms door — do not widen without a new decision).
     * @returns {Promise<void>}
     */
    async restrikeDuck(atSilence, { edgeMs = DECLICK_EDGE_MS } = {}) {
      const run = async () => {
        try {
          await atSilence?.();
        } catch (err) {
          log(`[restrike] trough work threw: ${err && err.message ? err.message : err}`);
        }
      };
      // No live output to duck (destroyed / no context): keep the visible re-strike honest by still
      // committing at the "trough", just with no audible fade. Never leave the caller hanging.
      if (destroyed || !masterGain || ctx.state === "closed") {
        await run();
        return;
      }
      const edgeSec = Math.max(0.001, edgeMs / 1000);
      const now = ctx.currentTime;
      const gain = masterGain.gain;
      // Fade unity → silence → unity as ONE raised-cosine value curve (declick.mjs). A single curve
      // (not two back-to-back edges) is deliberate: Web Audio rejects a second curve that touches the
      // first's end ("overlaps another curve"). cancelScheduledValues clears any half-finished duck
      // from a rapid double re-strike so the ramps can't fight; the curve then spans both edges
      // (2·edgeSec) starting from unity, its silent midpoint the trough where we commit. No
      // setValueAtTime anchor around it — an instantaneous event touching the curve's window is
      // itself rejected as an overlap, and the curve carries its own start (1) and end (1) values.
      gain.cancelScheduledValues(now);
      gain.setValueCurveAtTime(raisedCosineDuckCurve(), now, edgeSec * 2);
      // Commit at the trough (≈ one edge in). setTimeout is coarse vs. the audio clock, but the
      // visible commit needs a beat, not sample accuracy (spec §6.3), and it lands inside the duck.
      await new Promise((resolve) => setTimeout(resolve, edgeMs));
      await run();
      // Let the up-edge finish before resolving, so a caller awaiting the duck sees the sound return.
      // The curve itself ends at unity (declick.mjs) — no trailing anchor: a setValueAtTime landing
      // inside the still-settling curve window would be rejected as an overlap. A subsequent
      // re-strike re-anchors from the current value, so steady-state gain stays 1 either way.
      await new Promise((resolve) => setTimeout(resolve, edgeMs));
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
      masterGain.disconnect();
      if (ownsContext) ctx.close().catch(() => {});
    },
  };

  return engine;
}

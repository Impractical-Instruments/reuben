// The reuben AudioWorkletProcessor ("reuben-engine") — the persistent render instance
// of the reuben web player (issue #224).
//
// Codes against `crates/reuben-web/src/bridge.rs` (the flat C-ABI lifecycle: alloc /
// set_document / stage_resource / construct / render / queue_control / destroy, planar
// 128-frame I/O at input_ptr()/output_ptr()). Control buffers arrive pre-encoded by
// `js/codec.mjs` against `crates/reuben-web/src/codec.rs`.
//
// Deliberately self-contained — no imports. AudioWorkletGlobalScope guarantees neither
// TextEncoder nor TextDecoder, so all text crosses this boundary as UTF-8 bytes: the
// main thread pre-encodes what comes in (document, resource keys) and decodes what goes
// out (log/error bytes). The tiny writeBytes/error helpers from js/loader.mjs are
// inlined here for the same reason (the P1 spike kept its worklet dependency-free too).
//
// Load-bearing patterns proven by the P1 spike (#223):
// - SYNCHRONOUS `new WebAssembly.Module(bytes)` from raw posted bytes: Chromium
//   silently drops a structured-cloned WebAssembly.Module to a worklet (messageerror,
//   no diagnostics), and async instantiate can stall while the context is suspended
//   (the render thread isn't pumping microtasks). The main-thread ~4 KB sync-compile
//   limit does not apply in a worklet.
// - Ctor dance: try `_initialize` then `__wasm_call_ctors` — toolchain-portable; the
//   current LLD synthesizes ctor calls into every export so neither may exist.
// - `port.onmessageerror` posts loudly: silent structured-clone failures were a P1 bite.
// - Re-wrap Float32Array views EVERY quantum: memory growth detaches old views; the
//   pointers themselves are stable (the shell's buffers are statics).

class ReubenEngineProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.ex = null; // WASM exports, non-null once the module message arrived
    this.memory = null;
    this.engineReady = false; // a construct() succeeded and no destroy since
    this.renderFailed = false; // a render() trapped; stop re-trapping, stay silent
    this.outPtr = 0;
    this.inPtr = 0;
    this.channels = 0;
    this.inputChannels = 0;
    this.blockSize = 0;
    this.port.onmessage = (e) => {
      try {
        this.onMessage(e.data);
      } catch (err) {
        this.postError(`worklet: ${err}`);
      }
    };
    // A structured-clone failure surfaces HERE, not in onmessage — without this
    // listener it is perfectly silent (P1 finding).
    this.port.onmessageerror = () =>
      this.postError("port messageerror: message could not be deserialized in the worklet");
  }

  postError(text, bytes) {
    this.port.postMessage({ type: "error", text, bytes });
  }

  postLog(text) {
    this.port.postMessage({ type: "log", text });
  }

  // --- Inlined helpers over the raw exports (see header for why not loader.mjs). ---
  // WASM-memory rule: create views AFTER any call that can grow memory (alloc,
  // construct, stage_resource); never cache a view across calls.

  /** alloc + copy into linear memory; caller must dealloc(ptr, bytes.length). */
  writeBytes(bytes) {
    const ptr = this.ex.alloc(bytes.length);
    if (bytes.length > 0) {
      new Uint8Array(this.memory.buffer, ptr, bytes.length).set(bytes);
    }
    return ptr;
  }

  /** Copy of the shell's last failure message as raw UTF-8 (main thread decodes). */
  errorBytes() {
    const len = this.ex.error_len();
    if (len === 0) return undefined;
    return new Uint8Array(this.memory.buffer, this.ex.error_ptr(), len).slice();
  }

  /** All miss keys joined with ", " as raw UTF-8 bytes (main thread decodes). */
  missKeysBytes() {
    const parts = [];
    let total = 0;
    const count = this.ex.miss_count();
    for (let i = 0; i < count; i++) {
      const len = this.ex.miss_key_len(i);
      parts.push(new Uint8Array(this.memory.buffer, this.ex.miss_key_ptr(i), len).slice());
      total += len;
    }
    const joined = new Uint8Array(total + Math.max(0, parts.length - 1) * 2);
    let pos = 0;
    for (let i = 0; i < parts.length; i++) {
      if (i > 0) {
        joined[pos++] = 0x2c; // ","
        joined[pos++] = 0x20; // " "
      }
      joined.set(parts[i], pos);
      pos += parts[i].length;
    }
    return joined;
  }

  // --- Message dispatch. ---

  onMessage(msg) {
    switch (msg.type) {
      case "module":
        this.onModule(msg);
        return;
      case "load":
        this.onLoad(msg);
        return;
      case "control":
        this.onControl(msg);
        return;
      case "destroy":
        if (this.ex) this.ex.destroy();
        this.engineReady = false;
        this.port.postMessage({ type: "destroyed" });
        return;
      default:
        this.postLog(`worklet: unknown message type "${msg.type}"`);
    }
  }

  onModule(msg) {
    const imports = {
      env: {
        // The module's one import: raw UTF-8 bytes out of linear memory, shipped to
        // the main thread for decoding (no TextDecoder in this scope).
        log: (ptr, len) => {
          if (!this.memory) {
            this.postLog("<log call before memory was available>");
            return;
          }
          const bytes = new Uint8Array(this.memory.buffer, ptr, len).slice();
          this.port.postMessage({ type: "log", bytes });
        },
      },
    };
    // Synchronous compile + instantiate from raw bytes, deliberately (see header).
    const module = new WebAssembly.Module(msg.bytes);
    const instance = new WebAssembly.Instance(module, imports);
    const ex = instance.exports;
    this.memory = ex.memory;
    // Raw cdylib: run static ctors ourselves so `inventory` registration happens.
    // Either hook may be absent (current LLD folds ctors into every export).
    if (typeof ex._initialize === "function") ex._initialize();
    else if (typeof ex.__wasm_call_ctors === "function") ex.__wasm_call_ctors();
    this.ex = ex;
    this.postLog(`module instantiated, registry: ${ex.registry_count()} operators`);
  }

  onLoad(msg) {
    if (!this.ex) {
      this.postError("load before module was instantiated");
      return;
    }
    const ex = this.ex;
    // Unconditional teardown: drops any live engine AND any stale staged bundle from
    // an earlier (possibly failed) load; the instance stays reusable.
    this.engineReady = false;
    this.renderFailed = false;
    ex.destroy();

    // Document — arrives pre-encoded as UTF-8 bytes (no TextEncoder here).
    const doc = msg.doc;
    const docPtr = this.writeBytes(doc);
    const docStatus = ex.set_document(docPtr, doc.length);
    ex.dealloc(docPtr, doc.length);
    if (docStatus !== 0) {
      this.postError("set_document rejected the document");
      return;
    }

    // Stage the COMPLETE pre-discovered bundle (discovery ran on the main thread).
    for (const entry of msg.bundle) {
      const keyPtr = this.writeBytes(entry.keyBytes);
      const dataPtr = this.writeBytes(entry.bytes);
      const status = ex.stage_resource(
        keyPtr,
        entry.keyBytes.length,
        entry.kind,
        dataPtr,
        entry.bytes.length,
      );
      ex.dealloc(keyPtr, entry.keyBytes.length);
      ex.dealloc(dataPtr, entry.bytes.length);
      if (status !== 0) {
        this.postError(`stage_resource ${entry.key} rejected`, this.errorBytes());
        return;
      }
    }

    // `sampleRate` is the AudioWorkletGlobalScope global.
    const status = ex.construct(sampleRate);
    if (status === 1) {
      this.postError("construct failed", this.errorBytes());
      return;
    }
    if (status === 2) {
      // Should be impossible: the bundle was the discovery instance's complete set.
      this.postError(
        `construct reported ${ex.miss_count()} unexpected miss(es) — ` +
          "discovery should have bundled everything; missing keys follow",
        this.missKeysBytes(),
      );
      return;
    }

    this.channels = ex.channels();
    this.inputChannels = ex.input_channels();
    this.blockSize = ex.block_size();
    if (this.blockSize !== 128) {
      // The engine block is pinned to the Web Audio render quantum; anything else
      // would need a drain adapter this processor deliberately does not have.
      this.postError(`block_size ${this.blockSize} != render quantum 128`);
      return;
    }
    this.outPtr = ex.output_ptr();
    this.inPtr = ex.input_ptr();
    this.engineReady = true;
    this.port.postMessage({
      type: "ready",
      channels: this.channels,
      inputChannels: this.inputChannels,
      blockSize: this.blockSize,
    });
  }

  onControl(msg) {
    if (!this.ex) {
      this.postLog("control dropped: module not instantiated yet");
      return;
    }
    // Pre-encoded by codec.mjs on the main thread — this side just ferries bytes.
    const buffer = msg.buffer;
    const ptr = this.writeBytes(buffer);
    const status = this.ex.queue_control(ptr, buffer.length);
    this.ex.dealloc(ptr, buffer.length);
    if (status !== 0) {
      // Not fatal — e.g. the engine may not be constructed yet; detail already went
      // up through the log import.
      this.postLog("queue_control rejected a message");
    }
  }

  // --- Render. One engine block per 128-frame quantum. ---

  process(inputs, outputs) {
    // Keep the node alive but silent until an instrument is ready (or after a render
    // trap — the page shows why; a dead processor would stop audio with no diagnosis).
    if (!this.engineReady || this.renderFailed) return true;

    const bs = this.blockSize;
    let hasInput = 0;
    const input = inputs[0];
    if (this.inputChannels > 0 && input && input.length > 0 && input[0].length > 0) {
      // Planar staging: input[ch * 128 + f]. Zero first, then copy what the graph
      // provides — missing channels stay silent. View re-wrapped every quantum.
      const inView = new Float32Array(this.memory.buffer, this.inPtr, this.inputChannels * bs);
      inView.fill(0);
      const n = Math.min(this.inputChannels, input.length);
      for (let ch = 0; ch < n; ch++) {
        inView.set(input[ch], ch * bs);
      }
      hasInput = 1;
    }

    let status;
    try {
      status = this.ex.render(hasInput);
    } catch (err) {
      // A panic inside render() traps here; its message already went up via the panic
      // hook -> log import. Mark failed so we stop re-trapping.
      this.renderFailed = true;
      this.postError(`render trapped: ${err}`);
      return true;
    }
    if (status !== 0) {
      // 1 = no live engine (e.g. destroyed mid-flight): emit silence, stay alive.
      return true;
    }

    // Planar output out[ch * 128 + f], `channels` wide. Re-wrap every quantum —
    // memory growth detaches old views; the pointer is a static's fixed offset.
    const out = new Float32Array(this.memory.buffer, this.outPtr, this.channels * bs);
    const dest = outputs[0];
    const last = this.channels - 1;
    for (let ch = 0; ch < dest.length; ch++) {
      const src = ch < last ? ch : last; // mono destination gets ch 0; mono engine fills both
      dest[ch].set(out.subarray(src * bs, (src + 1) * bs));
    }
    return true;
  }
}

registerProcessor("reuben-engine", ReubenEngineProcessor);

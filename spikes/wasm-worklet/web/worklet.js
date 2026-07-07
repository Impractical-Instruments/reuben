// Throwaway spike (issue #223) — the AudioWorkletProcessor half of the boundary.
//
// The main thread compiles the WASM module (a worklet can't fetch) and posts it in;
// this processor instantiates it, runs the static ctors, calls init(), then does one
// render() per 128-frame quantum. Everything diagnostic goes up through port.postMessage
// so failures land on the page, never in a silent dead processor (design note on #223).

// The Web Audio render quantum; the engine's block_size is pinned to match, so one
// render() fills exactly one process() and no drain adapter is needed.
const QUANTUM = 128;
const CHANNELS = 2;

class ReubenSpikeProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.exports = null; // non-null once init() has succeeded
    this.memory = null;
    this.outPtr = 0;
    this.failed = false;
    this.blocks = 0; // rendered-block count, posted periodically as a liveness signal
    this.port.onmessage = (e) => {
      try {
        this.onMessage(e.data);
      } catch (err) {
        this.fail(`worklet setup: ${err}`);
      }
    };
    // A structured-clone failure surfaces HERE, not in onmessage — without this listener
    // it is perfectly silent (observed: Chromium drops a cloned WebAssembly.Module on
    // the floor this way, which is why the main thread sends raw bytes instead).
    this.port.onmessageerror = () =>
      this.fail("port messageerror: message could not be deserialized in the worklet");
  }

  onMessage(msg) {
    if (msg.type !== "module") return;
    const imports = {
      env: {
        // The module's one import: raw UTF-8 bytes out of linear memory, shipped to the
        // main thread for decoding (TextDecoder isn't guaranteed in this global scope).
        log: (ptr, len) => {
          if (!this.memory) {
            // Only reachable if a future toolchain emits a `start` section that logs
            // during instantiation (none today); don't eat the event silently. Plain
            // text, not bytes: TextEncoder isn't guaranteed in this scope either.
            this.port.postMessage({
              type: "log",
              text: "<log call before memory was available>",
            });
            return;
          }
          const bytes = new Uint8Array(this.memory.buffer, ptr, len).slice();
          this.port.postMessage({ type: "log", bytes });
        },
      },
    };
    // SYNCHRONOUS compile + instantiate, deliberately, from raw bytes:
    // - bytes, not a precompiled Module: Chromium fails to deliver a structured-cloned
    //   WebAssembly.Module to a worklet (silent messageerror — observed; see above).
    // - sync, not `await WebAssembly.instantiate`: while the context is suspended the
    //   render thread isn't pumping microtasks, so the promise may not resolve until
    //   after ctx.resume(). The main-thread-only sync-compile size limit (~4 KB) does
    //   not apply in a worklet.
    const module = new WebAssembly.Module(msg.bytes);
    const instance = new WebAssembly.Instance(module, imports);
    const ex = instance.exports;
    this.memory = ex.memory;

    // Raw cdylib (no wasm-bindgen): run the static ctors OURSELVES so `inventory`'s
    // life-before-main registration happens — this ordering is load-bearing (#223).
    // rustc/lld may name the hook `_initialize` or `__wasm_call_ctors`, or (current
    // toolchain) synthesize ctor calls into every export, in which case neither exists
    // and the init() call below runs them. registry_count() makes the truth loud.
    if (typeof ex._initialize === "function") ex._initialize();
    else if (typeof ex.__wasm_call_ctors === "function") ex.__wasm_call_ctors();

    const status = ex.init(sampleRate, msg.instrument);
    if (status !== 0) {
      const reason = new Uint8Array(
        this.memory.buffer,
        ex.error_ptr(),
        ex.error_len(),
      ).slice();
      this.fail("init failed (see reason)", reason);
      return;
    }
    this.outPtr = ex.output_ptr();
    this.exports = ex;
    this.port.postMessage({
      type: "ready",
      sampleRate,
      registryCount: ex.registry_count(),
    });
  }

  fail(text, bytes) {
    this.failed = true;
    this.port.postMessage({ type: "error", text, bytes });
  }

  process(inputs, outputs) {
    // Keep the node alive but silent until init has succeeded (or after a failure —
    // the page shows why; a dead processor would just stop audio with no diagnosis).
    if (!this.exports || this.failed) return true;
    let status;
    try {
      status = this.exports.render();
    } catch (err) {
      // A panic inside render() traps here. Its message already went up via the panic
      // hook -> log import; this marks the processor failed so we stop re-trapping.
      this.fail(`render trapped: ${err}`);
      return true;
    }
    if (status !== 0) {
      this.fail(`render returned status ${status}`);
      return true;
    }
    // Liveness heartbeat (~every 2.7 s at 48 kHz): proves the audio thread is still
    // pulling us — the difference between "silent because muted" and "processor dead"
    // when debugging on a phone.
    this.blocks++;
    if (this.blocks % 1024 === 1) {
      this.port.postMessage({ type: "blocks", count: this.blocks });
    }
    // Re-wrap the view every quantum: memory growth detaches previous ArrayBuffer views.
    // The pointer itself is a static's fixed offset, fetched once at init.
    const out = new Float32Array(this.memory.buffer, this.outPtr, CHANNELS * QUANTUM);
    const chans = outputs[0];
    for (let ch = 0; ch < chans.length; ch++) {
      // Planar layout [ch0 × 128, ch1 × 128]; a mono destination gets channel 0.
      const src = ch < CHANNELS ? ch : CHANNELS - 1;
      chans[ch].set(out.subarray(src * QUANTUM, (src + 1) * QUANTUM));
    }
    return true;
  }
}

registerProcessor("reuben-spike", ReubenSpikeProcessor);

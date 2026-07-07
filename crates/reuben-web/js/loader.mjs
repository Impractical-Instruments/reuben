// Environment-agnostic helpers over the raw reuben-web WASM exports (issue #224).
//
// Codes against `crates/reuben-web/src/bridge.rs` — the flat C-ABI: alloc/dealloc,
// set_document, stage_resource, construct, miss_* readers, error_ptr/error_len,
// channels/input_channels/block_size, destroy. Runs on the main thread OR in Node
// (both have TextEncoder/TextDecoder); the AudioWorklet deliberately does NOT import
// this file — it inlines its own copies (worklet scope guarantees neither).
//
// WASM-memory rule observed throughout: views are created AFTER any call that can grow
// linear memory (alloc, construct, stage_resource) and are never cached across calls —
// growth detaches old ArrayBuffer views, so every read re-wraps from ex.memory.buffer.
// The pointers themselves are stable (the shell's buffers are statics).

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/**
 * Allocate `bytes.length` bytes in WASM linear memory and copy `bytes` in.
 * Returns the pointer; the caller must `ex.dealloc(ptr, bytes.length)` when done.
 *
 * @param {WebAssembly.Exports} ex - the module's exports (needs alloc + memory)
 * @param {Uint8Array} bytes
 * @returns {number} pointer into linear memory (0 for empty input)
 */
export function writeBytes(ex, bytes) {
  const ptr = ex.alloc(bytes.length);
  if (bytes.length > 0) {
    // View created after alloc (which can grow memory), never before.
    new Uint8Array(ex.memory.buffer, ptr, bytes.length).set(bytes);
  }
  return ptr;
}

/**
 * Read the shell's last failure message (empty string when the last op succeeded).
 *
 * @param {WebAssembly.Exports} ex
 * @returns {string}
 */
export function readError(ex) {
  const len = ex.error_len();
  if (len === 0) return "";
  return decoder.decode(new Uint8Array(ex.memory.buffer, ex.error_ptr(), len));
}

/**
 * Read the misses recorded by the last construct attempt.
 *
 * @param {WebAssembly.Exports} ex
 * @returns {Array<{key: string, kind: number}>} canonical root-relative keys
 *   (e.g. "voices/sampler-voice.json") and kinds (0 = text/JSON, 1 = WAV sample)
 */
export function readMisses(ex) {
  const count = ex.miss_count();
  const misses = [];
  for (let i = 0; i < count; i++) {
    const len = ex.miss_key_len(i);
    // Re-wrap per miss: cheap, and immune to any future export growing memory.
    const key = decoder.decode(new Uint8Array(ex.memory.buffer, ex.miss_key_ptr(i), len));
    misses.push({ key, kind: ex.miss_kind(i) });
  }
  return misses;
}

/** Stage one resource; throws (with the shell's error) on rejection. */
function stageResource(ex, key, kind, bytes) {
  const keyBytes = encoder.encode(key);
  const keyPtr = writeBytes(ex, keyBytes);
  // The second alloc may grow memory; keyPtr stays valid (pointers are stable), only
  // views would detach — and none are held here.
  const dataPtr = writeBytes(ex, bytes);
  const status = ex.stage_resource(keyPtr, keyBytes.length, kind, dataPtr, bytes.length);
  ex.dealloc(keyPtr, keyBytes.length);
  ex.dealloc(dataPtr, bytes.length);
  if (status !== 0) {
    const reason = readError(ex);
    throw new Error(`stage_resource ${key} rejected${reason ? `: ${reason}` : ""}`);
  }
}

/**
 * Run the full fetch-on-miss lifecycle: set_document, then loop construct() —
 * fetching and staging each reported miss — until the engine is ready or fails.
 *
 * @param {WebAssembly.Exports} ex - a live instance's exports
 * @param {string} docText - the top-level instrument document (UTF-8 JSON text)
 * @param {number} sampleRate
 * @param {(key: string, kind: number) => Promise<Uint8Array>} fetchResource - fetches
 *   the bytes for one canonical root-relative key (kind: 0 = text, 1 = WAV sample)
 * @returns {Promise<{channels: number, inputChannels: number, blockSize: number}>}
 */
export async function loadInstrument(ex, docText, sampleRate, fetchResource) {
  const docBytes = encoder.encode(docText);
  const docPtr = writeBytes(ex, docBytes);
  const docStatus = ex.set_document(docPtr, docBytes.length);
  ex.dealloc(docPtr, docBytes.length);
  if (docStatus !== 0) {
    throw new Error("set_document rejected the document (not UTF-8?)");
  }

  let previousRound = null;
  for (;;) {
    const status = ex.construct(sampleRate);
    if (status === 0) {
      return {
        channels: ex.channels(),
        inputChannels: ex.input_channels(),
        blockSize: ex.block_size(),
      };
    }
    if (status === 1) {
      throw new Error(readError(ex) || "construct failed (no error message)");
    }
    // status 2: misses. Livelock guard — if this round's miss set is identical to the
    // previous round's, staging isn't taking (wrong key, bad bytes) and looping forever
    // would just re-fetch the same resources.
    const misses = readMisses(ex);
    const round = misses.map((m) => `${m.kind}:${m.key}`).sort().join("\n");
    if (round === previousRound) {
      throw new Error(
        `resources did not stage: ${misses.map((m) => m.key).join(", ")}`,
      );
    }
    previousRound = round;

    for (const { key, kind } of misses) {
      const fetched = await fetchResource(key, kind);
      const bytes = fetched instanceof Uint8Array ? fetched : new Uint8Array(fetched);
      stageResource(ex, key, kind, bytes);
    }
  }
}

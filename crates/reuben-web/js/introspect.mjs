// introspect.mjs — generic wasm introspection over a live module instance's C-ABI exports
// (bridge.rs, ADR-0052 §2). NOT agent logic: this is the stateless authoring surface
// (describe_operators, describe_instrument, validate, content_hash) any consumer of the wasm
// engine can bind — the in-page tool layer (tools.mjs) is one such consumer, check.mjs another.
//
// Error-layer discipline (ADR-0048 §3): a call throws ONLY when the export "could not do its
// job" — an unknown operator, a document that fails to load, bad UTF-8. A {ok:false} validate
// report is the export WORKING and is returned, never thrown.

import { writeBytes, readError, readReport } from "./loader.mjs";

const encoder = new TextEncoder();

/**
 * Build the real introspection adapter over a live wasm exports instance (bridge.rs's C-ABI).
 * Each call writes the input into linear memory, invokes the export, and reads the result via
 * the loader.mjs helpers (which re-wrap memory views after every call — growth detaches old
 * views; ADR-0040 §3). The returned adapter is the `introspect` createToolLayer expects.
 *
 * @param {WebAssembly.Exports} ex - a live module instance's exports
 * @returns {{describeOperators: (name?: string) => object[],
 *            describeInstrument: (docText: string) => object,
 *            validate: (docText: string) => object,
 *            contentHash: (docText: string) => string}}
 */
export function wasmIntrospect(ex) {
  // Write `text` into linear memory, call the (ptr,len) export, free the buffer, return the rc.
  // Used for both document and operator-name inputs — anything the export takes as (ptr,len).
  const callWithText = (fn, text) => {
    const bytes = encoder.encode(text);
    const ptr = writeBytes(ex, bytes);
    const rc = fn(ptr, bytes.length);
    ex.dealloc(ptr, bytes.length);
    return rc;
  };
  return {
    describeOperators(name) {
      // Absent name ⇒ call with (0, 0): name_len 0 lists the whole registry (bridge.rs).
      const rc =
        name == null || name === ""
          ? ex.describe_operators(0, 0)
          : callWithText(ex.describe_operators, name);
      if (rc !== 0) throw new Error(readError(ex) || "describe_operators failed");
      return JSON.parse(readReport(ex)).operators;
    },
    describeInstrument(docText) {
      const rc = callWithText(ex.describe_instrument, docText);
      if (rc !== 0) throw new Error(readError(ex) || "describe_instrument failed");
      return JSON.parse(readReport(ex));
    },
    validate(docText) {
      // rc 1 is the one call-level failure (bad UTF-8, unexpected); rc 0 always carries a
      // Report — INCLUDING {ok:false}, which is a successful call (ADR-0048 §3), never a throw.
      const rc = callWithText(ex.validate, docText);
      if (rc === 1) throw new Error(readError(ex) || "validate failed (bad UTF-8)");
      return JSON.parse(readReport(ex));
    },
    contentHash(docText) {
      const rc = callWithText(ex.content_hash, docText);
      if (rc !== 0) throw new Error(readError(ex) || "content_hash failed");
      return readReport(ex); // the opaque token, a plain string (not JSON)
    },
  };
}

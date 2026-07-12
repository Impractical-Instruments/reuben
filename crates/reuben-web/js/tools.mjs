// tools.mjs — the in-page tool layer (issue #353, ADR-0052 §2) the chat agent binds: one tool
// per ADR-0048 contract, the SAME report shapes as the native MCP lane (ADR-0052 §5, "one
// schema, two doors"), over the existing C-ABI worklet + engine API. This is a CONSUMER of the
// eight contracts and nothing else.
//
// Two exports:
//   - wasmIntrospect(ex): the real adapter over a wasm exports instance, using loader.mjs's
//     memory-view helpers — the four stateless authoring exports (describe_operators,
//     describe_instrument, validate, content_hash).
//   - createToolLayer({ engine, introspect }): the eight contract tools, keyed by their EXACT
//     snake_case names (the names M1's agent schemas use). Engine-bound tools (send/swap/
//     engine_status/get_current_instrument) speak to js/reuben-engine.mjs; the authoring tools
//     delegate to `introspect` (a wasmIntrospect in production, a fake in tests).
//
// Error-layer discipline (ADR-0048 §3): a tool throws ONLY when it "could not do its job"
// (isError) — an unknown operator, a document that fails to load, an unreachable engine. A
// {ok:false} validate/swap report is the tool WORKING and is returned, never thrown.

import { writeBytes, readError, readReport } from "./loader.mjs";
import { structuralDiff } from "./diff.mjs";

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

/**
 * Build the eight-contract tool layer over an engine (reuben-engine.mjs) and an introspection
 * adapter (wasmIntrospect, or a fake in tests). The returned object's keys are the EXACT
 * ADR-0048 contract names in snake_case.
 *
 * @param {object} deps
 * @param {import("./reuben-engine.mjs")} deps.engine - a live reuben engine (send, loadBundle,
 *   currentBundle, context, node)
 * @param {ReturnType<typeof wasmIntrospect>} deps.introspect - the authoring adapter
 * @returns {object} the tool layer, keyed by ADR-0048 contract name
 */
export function createToolLayer({ engine, introspect }) {
  // A `document` argument may arrive as a parsed object OR a JSON string. toText normalizes to
  // the wire text the wasm/engine consume; toDoc parses back to an object for the diff.
  const toText = (document) => (typeof document === "string" ? document : JSON.stringify(document));
  const toDoc = (x) => (typeof x === "string" ? JSON.parse(x) : x);

  // Reachability probe (ADR-0048 §5): the page IS the host, so "reachable" = a live worklet node
  // and an AudioContext that isn't closed. Shared by send (probe-first) and engine_status so the
  // two can never disagree on what "reachable" means.
  const reachable = () => !!engine.node && engine.context?.state !== "closed";

  // The get_diagnostics counter SEAM (ADR-0052 §2: "the web shell grows counters when the chat
  // lands"). The worklet has no xrun/ring instrumentation yet, so these are honest zeros
  // ("nothing instrumented yet"), NOT a stub. The field names mirror
  // reuben_core::coordinator::DiagnosticsReport EXACTLY — the shape is single-sourced against
  // the native type; future instrumentation writes into this object.
  const diagnostics = {
    output_xruns: 0,
    input_ring_underruns: 0,
    input_ring_overruns: 0,
    input_ring_producer_drops: 0,
  };

  return {
    /** describe_operators({name?}) → {operators}. Unknown name throws (isError). */
    describe_operators({ name } = {}) {
      return { operators: introspect.describeOperators(name) };
    },

    /** describe_instrument({document}) → PatchBoundary. A doc that fails to load throws (isError). */
    describe_instrument({ document }) {
      return introspect.describeInstrument(toText(document));
    },

    /** validate({document}) → Report. NEVER throws for a bad doc — {ok:false} is the result (ADR-0048 §3). */
    validate({ document }) {
      return introspect.validate(toText(document));
    },

    /**
     * send({messages}) → {sent: N}. Batch is the natural authoring gesture (ADR-0048 §5); a
     * non-empty array is required (min 1). Each message is one sequential control datagram.
     */
    send({ messages } = {}) {
      if (!Array.isArray(messages) || messages.length === 0) {
        throw new Error("send requires a non-empty messages array (min 1)");
      }
      // Probe-first (ADR-0048 §5): posting to a torn-down node is not "sent". An unreachable
      // engine is isError (a throw), not a silent no-op that reports a false `sent` count.
      if (!reachable()) throw new Error("send: engine is not reachable");
      for (const { address, args = [] } of messages) {
        engine.send(address, args);
      }
      return { sent: messages.length };
    },

    /**
     * swap({document, resources?, expect?}) → Report + content_hash + (on install) diff.
     *
     * Web swap is the restart-swap Toy-switch path (ADR-0052 §2): the document installs BY VALUE
     * (there is no disk), and the diff is the structural node-identity diff (spec §4.6), never
     * native's survivor/state_reset stats — but `survived: 0` is still reported (restart-swap
     * honesty). `expect` is ADR-0046 §9's opt-in guard; a rejected swap (conflict or {ok:false})
     * installs nothing and the old sound keeps playing.
     */
    async swap({ document, resources, expect } = {}) {
      const text = toText(document);
      const beforeText = engine.currentBundle()?.docText ?? null;
      const installedHash = beforeText ? introspect.contentHash(beforeText) : "";

      // expect guard: a stale token means the client's view of what's installed is wrong — no
      // install; the caller re-reads via get_current_instrument and reconciles (ADR-0046 §9).
      if (expect != null && expect !== installedHash) {
        return {
          ok: false,
          errors: [],
          warnings: [],
          content_hash: installedHash,
          conflict: { expected: expect, actual: installedHash },
        };
      }

      const report = introspect.validate(text);
      if (!report.ok) {
        // Rejected: nothing installed; content_hash names what KEEPS playing (ADR-0048 §5).
        return { ...report, content_hash: installedHash };
      }

      // Install by value — the restart-swap construct (destroy → stage → construct in the
      // worklet, ADR-0052 §2). loadBundle takes the document verbatim.
      await engine.loadBundle({ docText: text, resources: resources ?? [] });
      const newHash = introspect.contentHash(text);
      const d = structuralDiff(beforeText ? toDoc(beforeText) : { nodes: [] }, toDoc(text));
      // survived: 0 ALWAYS (restart-swap; ADR-0052 §2). `d` is {added, removed, changed}; no
      // state_reset key — degenerate on web, deliberately not computed (#353) — the web diff IS
      // the structural node-identity diff.
      //
      // `restarted`: true iff a bundle was ALREADY loaded before this install (beforeText
      // non-null) AND audio was actually running — a genuine SONIC restart, distinct both from a
      // first install into silence and from a change made while paused. Spec §6.4 scopes the
      // once-per-session re-strike line to "only when the instrument is actively playing": a user
      // who suspends the AudioContext (nothing sounding) with a bundle still loaded and then makes
      // a structural change has no restart to be honest about. This is the signal agent-host.mjs
      // (#356) uses to gate that line; the AudioContext state is the same "is anything sounding?"
      // handle `reachable`/engine_status already read.
      const playing = engine.context?.state === "running";
      return {
        ...report,
        content_hash: newHash,
        diff: { survived: 0, ...d },
        restarted: beforeText !== null && playing,
      };
    },

    /**
     * engine_status() → {reachable, state}. The page IS the host (ADR-0052 §2), so native's
     * endpoints/sidecar are meaningless in-page — this anchor reports AudioContext state instead
     * ("running"/"suspended"/"closed", spec §0.2). Never throws — answering "reachable?" is its job.
     */
    engine_status() {
      return { reachable: reachable(), state: engine.context?.state };
    },

    /**
     * get_current_instrument() → {document, content_hash} — the staged document the shell owns
     * (ADR-0052 §2). Nothing loaded ⇒ throws (isError), analogous to native's engine-unreachable.
     */
    get_current_instrument() {
      const b = engine.currentBundle();
      if (!b?.docText) {
        throw new Error("no instrument is loaded");
      }
      return { document: toDoc(b.docText), content_hash: introspect.contentHash(b.docText) };
    },

    /** get_diagnostics() → the four DiagnosticsReport counters (the ADR-0052 §2 seam; see above). */
    get_diagnostics() {
      return { ...diagnostics };
    },
  };
}

// chat/reshape.js — the ROUTING seam (issue #397): it drives ONE live agent turn (js/chat-host.mjs
// `runTurn`) into the change-card controller the spine already built (spine.js `beginReshapeCard`),
// exactly as the crafted-envelope specs drive it — but from real model output. This is where the
// resolved turn envelope (js/agent-turn.mjs) becomes a §6.1 param `resolve` (a live sweep) or a §6.2
// structural `restrike` (the declicked re-strike), plus the §4.1 value-sweep and the §5 failure
// shapes. Kept OUT of main.js's bulk and free of the surface machinery (injected as callbacks) so a
// stubbed-transport test can drive it with no key and no network.
//
// Routing signal (agent-host.mjs): `turn.diff` is set ONLY by a `swap` → structural (restrike). A
// param reshape routes through `send` (§6.1) and carries no diff → the touched controls + their new
// values are synthesized from the send messages (paramSweepFromToolLog). A turn that touches neither
// changed no sound → §5.2 chat turn. A `{ok:false}` swap is repaired inside the host loop (the model
// sees the report and self-corrects, ADR-0048 §3) — it never reaches here as a failure.

import { nodeOfControl } from "./board.js";

const NO_KNOB_DIFF = { survived: 0, added: [], removed: [], changed: [] };

/**
 * Synthesize the §4.1 glow + value-sweep for a PARAM (send-only) reshape from a resolved turn's
 * toolLog. agent-host.mjs attaches `turn.diff` only on a swap, so a param reshape's touched controls
 * and their new values live in the `send` tool inputs instead. Returns the fresh widget set (each
 * touched widget's `default` folded to the sent value, so `board.update` re-renders it at the new
 * position — board.js `sameWidget` keys on `default`) and a node-identity diff for the glow. A send
 * to an address with no exposed control is a no-knob change (§4.3): it drops out of the sweep.
 *
 * @param {import("../../../crates/reuben-web/js/agent-turn.mjs").ToolInvocation[]} toolLog
 * @param {object[]} widgets - the current surface widgets (the sweep's base).
 * @returns {{ widgets: object[], diff: object, touched: number }}
 */
export function paramSweepFromToolLog(toolLog, widgets) {
  const sent = new Map(); // control address -> latest numeric value sent this turn
  for (const inv of toolLog ?? []) {
    if (inv.name !== "send" || inv.isError) continue;
    for (const m of inv.input?.messages ?? []) {
      const v = Array.isArray(m.args) ? m.args[0] : m.args;
      if (typeof m.address === "string" && typeof v === "number") sent.set(m.address, v);
    }
  }
  if (sent.size === 0) return { widgets: widgets ?? [], diff: NO_KNOB_DIFF, touched: 0 };

  const changed = new Set();
  const swept = (widgets ?? []).map((w) => {
    if (sent.has(w.address)) {
      changed.add(nodeOfControl(w.address));
      return { ...w, default: sent.get(w.address) };
    }
    return w;
  });
  return {
    widgets: swept,
    diff: { survived: 0, added: [], removed: [], changed: [...changed] },
    touched: changed.size,
  };
}

/**
 * Drive one live turn through the change-card (spec §4.2 streaming, §6 re-strike, §5 failure). The
 * caller supplies the spine `api`, an assembled `host` (js/chat-host.mjs), the live `engine`, and the
 * surface callbacks main.js owns.
 *
 * @param {object} o
 * @param {string} o.text - the user's plain-language turn.
 * @param {object} o.api - the spine api (beginReshapeCard, chatReply, board, transcript, setTurnInFlight).
 * @param {{ runTurn: Function }} o.host - the assembled chat host.
 * @param {object} o.engine - the live worklet engine (context, currentBundle).
 * @param {() => (object[]|null)} o.resolveWidgets - re-resolve widgets from the engine's CURRENT
 *   (post-swap) document — the structural value-sweep's fresh widget set.
 * @param {() => object[]} o.currentWidgets - the current surface widgets (the param sweep's base).
 * @param {(widgets: object[]) => void} [o.onWidgets] - fold the swept widget set back into the
 *   shell's surface tracking so subsequent turns build on it.
 * @param {(err: Error) => void} [o.onTerminalFailure] - the §5.3 phase-specific terminal handler: a
 *   reshape keeps the prior sound (chat-only), a first creation lands back at the gallery.
 * @param {boolean} [o.pushUserLine] - push the user's "you" line before the card (true for a typed
 *   reshape; false for the describe path, whose line is already seeded, §2.4).
 * @returns {Promise<import("../../../crates/reuben-web/js/agent-turn.mjs").AgentTurn|null>}
 */
export async function runReshapeTurn({
  text,
  api,
  host,
  engine,
  resolveWidgets,
  currentWidgets,
  onWidgets,
  onTerminalFailure,
  pushUserLine = true,
}) {
  // Was anything actually SOUNDING before the turn? Captured up front: a swap installs AND starts
  // the new instrument, so reading the state afterward always says "running" and would defeat
  // §6.4's build-and-be-ready path. A reshape (a bundle loaded + context running) ducks; a first
  // install into silence does not.
  const wasSounding = engine?.context?.state === "running" && engine?.currentBundle() != null;

  if (pushUserLine) api.transcript.push({ role: "you", text });
  api.setTurnInFlight(true);
  const card = api.beginReshapeCard();

  try {
    let resolved;
    try {
      resolved = await host.runTurn(text, { onPlanDelta: (t) => card.appendPlan(t) });
    } catch (err) {
      // Host/transport failure (ADR-0054 §6) collapses to §5.3's terminal shape — the host reason is
      // NEVER shown to the user (§5.1). The card was mounted for streaming; settle it plan-only so it
      // doesn't hang in "thinking", then hand the phase-specific terminal copy to the caller.
      card.resolve();
      onTerminalFailure?.(err);
      return null;
    }

    if (resolved.diff) {
      // §6.2 STRUCTURAL: a swap ran. Re-resolve the surface from the NEW document for the value-sweep,
      // then re-strike — the sweep + card-commit + playhead reset land co-timed at the duck's trough
      // (§6.2.1). `sounding=false` (a first install into silence) skips the duck (build-and-be-ready).
      const widgets = resolveWidgets?.();
      const sweep = widgets
        ? () => {
            api.board.update(widgets, engine);
            onWidgets?.(widgets);
          }
        : undefined;
      await card.restrike(resolved.diff, resolved.restartHonesty, { sounding: wasSounding, sweep });
      return resolved;
    }

    // No swap. Either a §6.1 PARAM reshape (send moved a live control) or a §5.2 no-change turn.
    const sweep = paramSweepFromToolLog(resolved.toolLog, currentWidgets?.() ?? []);
    if (sweep.touched > 0) {
      const apply = () => {
        api.board.update(sweep.widgets, engine);
        onWidgets?.(sweep.widgets);
      };
      card.resolve(sweep.diff, { sweep: apply });
      return resolved;
    }

    // Nothing changed the sound: a §5.2 chat turn — unsatisfiable's "nearest thing" (case 2), the
    // empty/off-topic re-orient (case 4), or a reshape whose repair was exhausted (§5.3, sound kept).
    // The streamed plan IS the reuben reply; resolve the card plan-only (no rows, no glow, no surface
    // or transport touch). NOTE (§5.2 strictly wants NO change-card for a chat turn): the transcript
    // is append-only, so once streaming has mounted the card we settle it plan-only rather than
    // convert to a bare chat line — the honest, visible-streaming compromise until a change-vs-chat
    // signal lets the mount defer.
    card.resolve();
    return resolved;
  } finally {
    api.setTurnInFlight(false);
  }
}

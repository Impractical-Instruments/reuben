// agent-turn.mjs — THE turn/message envelope (issue #354): the data contract between the chat
// agent (js/agent-host.mjs) and the change-card + transcript UI (issue #358 renders against this;
// keep it stable). It is a PURE data shape + a builder + the §4.2 streaming state transitions —
// no DOM, no engine, no network. The UI ticket owns rendering; this owns the contract.
//
// Provenance of every field (spec §-numbers are docs/web-chat-authoring-ux-spec.md):
//   - `plan`         the streamed plan text, incrementally appended as tokens arrive (§4.2:
//                    "a streaming plan that resolves in place into the card"). LOAD-BEARING —
//                    the card renders the plan AS it streams, not a final blob.
//   - `diff`         the resolved structural node-identity diff (§4.6), taken verbatim from a
//                    `swap` tool result's `diff` ({survived, added, removed, changed}). `survived`
//                    is ALWAYS 0 on web (restart-swap, ADR-0052 §2). null until a swap resolves.
//   - `caveats`      RESERVED slot (§6.1: the send-vs-swap caveat that shrinks the re-strike).
//   - `alternatives` RESERVED slot (§5.1: alternative-interpretation chips).
//   - `restartHonesty` RESERVED slot (§4.7 is D's slot, §6.4 is F's content: the restart-honesty
//                    line). Shape defined here; CONTENT is filled by later tickets (#356/M2).
//   - `toolLog`      the tool_use/tool_result record for THIS turn — internal provenance for the
//                    card (which tools ran, what they returned). NOT user-facing prose; a tool
//                    error lands here (surfaced to the agent), never as a user-facing failure (M2).
//   - `status`       the §4.2 lifecycle: "thinking" (created, plan streaming) → "resolved"
//                    (resolved in place). "failed" is RESERVED for M2's failure taxonomy (§5.3) —
//                    this plumbing ticket never produces it.
//
// The reserved slots are deliberately empty here (the shape, not the copy): filling them is the
// authoring-policy ticket's job (#356). #358 can bind to them today knowing the shape won't move.

/**
 * The structural node-identity diff a resolved reshape carries (spec §4.6, js/diff.mjs). Verbatim
 * from a successful `swap` result's `diff`. Every address appears in at most one of add/remove/change.
 * @typedef {Object} StructuralDiff
 * @property {number} survived   Always 0 on web (restart-swap honesty, ADR-0052 §2).
 * @property {string[]} added    Node addresses present in the new document but not the old.
 * @property {string[]} removed  Node addresses present in the old document but not the new.
 * @property {string[]} changed  Node addresses present in both whose content differs.
 */

/**
 * One alternative-interpretation chip (spec §5.1). Shape only — CONTENT is a later ticket's job.
 * @typedef {Object} AlternativeChip
 * @property {string} id     Stable id the UI keys on / the agent re-selects by.
 * @property {string} label  Sensory phrase for the alternative reading (filled by #356; §1 lexicon).
 */

/**
 * One tool round-trip recorded for the card's provenance (internal, not user-facing prose).
 * @typedef {Object} ToolInvocation
 * @property {string} id       The model's tool_use id.
 * @property {string} name     The ADR-0048 contract name (snake_case).
 * @property {*} input         The tool input the model produced.
 * @property {*} [result]      The tool's return value (present once resolved), or the error text.
 * @property {boolean} isError Whether the tool could-not-do-its-job (ADR-0048 §3). Surfaced to the
 *                             AGENT as a tool_result, never to the user (failure UX is M2).
 */

/**
 * THE turn envelope. One per conversational turn. A `user` turn is just `plan` text; an `assistant`
 * turn carries the streamed plan, the resolved diff, and the reserved slots.
 * @typedef {Object} AgentTurn
 * @property {string} id
 * @property {"user"|"assistant"} role
 * @property {"thinking"|"resolved"|"failed"} status
 * @property {string} plan
 * @property {StructuralDiff|null} diff
 * @property {string[]} caveats
 * @property {AlternativeChip[]} alternatives
 * @property {string|null} restartHonesty
 * @property {ToolInvocation[]} toolLog
 */

let __seq = 0;
/** Monotonic per-session turn id. Not cryptographic — just stable + ordered for the transcript. */
function nextTurnId() {
  __seq += 1;
  return `turn-${__seq}`;
}

/**
 * F's CONTENT for D's reserved `restartHonesty` slot (issue #356, spec §6.4): the one light,
 * positive framing line the FIRST structural restart of an already-playing sound gets in a
 * session — "wordless on every repeat" after. `agent-host.mjs` is the session-scoped gate (once
 * per `createAgentHost` instance, and only when a swap's `restarted` flag says something was
 * genuinely already playing — js/tools.mjs). Deliberately register-neutral (no jargon either
 * pairing needs to swap out) so it never needs to branch on §8's plain/theory-aware state.
 */
export const RESTART_HONESTY_LINE = "Here's the new version, from the top.";

/**
 * A user turn: plain text, already resolved (the human's input needs no streaming/diff).
 * @param {string} text
 * @returns {AgentTurn}
 */
export function userTurn(text) {
  return {
    id: nextTurnId(),
    role: "user",
    status: "resolved",
    plan: text,
    diff: null,
    caveats: [],
    alternatives: [],
    restartHonesty: null,
    toolLog: [],
  };
}

/**
 * Build an assistant turn in the "thinking" state (spec §4.2) with a mutable, appendable plan and
 * the reserved slots. The host drives it: `appendPlan` per token, `recordTool` per tool round-trip,
 * `setDiff` when a swap resolves, then `resolve()` to transition thinking → resolved-in-place.
 *
 * @returns {{
 *   turn: AgentTurn,
 *   appendPlan: (text: string) => void,
 *   recordTool: (inv: ToolInvocation) => void,
 *   setDiff: (diff: StructuralDiff) => void,
 *   setRestartHonesty: (line: string) => void,
 *   resolve: () => AgentTurn,
 * }}
 */
export function assistantTurn() {
  /** @type {AgentTurn} */
  const turn = {
    id: nextTurnId(),
    role: "assistant",
    status: "thinking",
    plan: "",
    diff: null,
    caveats: [],
    alternatives: [],
    restartHonesty: null,
    toolLog: [],
  };
  return {
    turn,
    // §4.2: append each streamed token to the plan in place — the card re-renders as it grows.
    appendPlan(text) {
      turn.plan += text;
    },
    // Record one tool round-trip's provenance (including a surfaced-to-agent error, ADR-0048 §3).
    recordTool(inv) {
      turn.toolLog.push(inv);
    },
    // Attach the resolved structural diff from a swap (spec §4.6). Last swap in a turn wins.
    setDiff(diff) {
      turn.diff = diff;
    },
    // Attach F's restart-honesty line (spec §6.4) — set by agent-host.mjs at most once per
    // session, only on a genuine first restart of already-playing sound.
    setRestartHonesty(line) {
      turn.restartHonesty = line;
    },
    // thinking → resolved-in-place (§4.2). Returns the frozen turn for the transcript.
    resolve() {
      if (turn.status === "thinking") turn.status = "resolved";
      return turn;
    },
  };
}

/**
 * A stub transcript sink — proves the loop without building the UI (issue #358 owns the real one).
 * Records the ordered turns and every streamed delta, so a test can assert the plan streamed
 * INCREMENTALLY (multiple deltas) rather than landing as one blob (spec §4.2). The four callbacks
 * are exactly the surface js/agent-host.mjs drives; #358's renderer implements the same shape.
 *
 * @returns {{
 *   turns: AgentTurn[],
 *   deltas: Array<{ id: string, text: string }>,
 *   onUserTurn: (turn: AgentTurn) => void,
 *   onTurnStart: (turn: AgentTurn) => void,
 *   onPlanDelta: (turn: AgentTurn, text: string) => void,
 *   onTurnResolved: (turn: AgentTurn) => void,
 * }}
 */
export function stubTranscript() {
  const turns = [];
  const deltas = [];
  return {
    turns,
    deltas,
    onUserTurn(turn) {
      turns.push(turn);
    },
    onTurnStart(turn) {
      turns.push(turn);
    },
    onPlanDelta(turn, text) {
      deltas.push({ id: turn.id, text });
    },
    onTurnResolved(_turn) {
      // Resolved in place: the turn object was pushed at onTurnStart and mutated since, so there
      // is nothing to append — this is the hook #358 uses to commit the change-card.
    },
  };
}

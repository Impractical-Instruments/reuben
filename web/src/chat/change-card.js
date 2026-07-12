// chat/change-card.js — "rendering the agent at work" (spec §4, issue #358). The transcript half
// of the A+B hybrid (spec §4.1): a compact, PERSISTENT change-card that is the scannable scroll-back
// record of one reshape. Its sibling is the surface highlight on chat/board.js — the two are LINKED
// (hover a card row → echo-highlight its control), and both are driven off the SAME structural
// node-identity diff (§4.6, js/diff.mjs). This module owns the card; the board owns the surface glow.
//
// The card is a STATEFUL component with ONE persistent DOM element (`el`). It renders against the
// turn envelope (js/agent-turn.mjs — the #354 contract; we render AGAINST it, never mutate its
// shape) and the host mutates that envelope in place: `update()` re-renders the card's innards from
// the current envelope WITHOUT replacing `el`. That is what "resolves in the SAME object" means
// (spec §4.2) — the transcript re-render reuses this same node, and thinking → resolved is a
// re-paint of its guts, never a second card appended.
//
// LIFECYCLE the card renders (envelope `status`, spec §4.2):
//   - "thinking": the streamed natural-language plan text, growing token by token (a working cue).
//     The instrument stays hand-live throughout (§3.4) — the card is inert chrome, it freezes nothing.
//   - "resolved": the plan stays as the lead summary and the card resolves IN PLACE into rows — one
//     sensory row per add / change / remove (§4.1), or a collapsed headline past the big-diff
//     threshold (§4.4). The restart-honesty foot (§4.7) renders if the envelope carries it (#360).
//
// SENSORY-ONLY (spec §4.5): the card IS the translation layer from the node-addressed diff to §1's
// sound/intent vocabulary. Rows must be forbidden-word-clean (the epic's lexicon gate). We do NOT
// have the agent's live per-change prose yet (its loop is not wired into the browser — the same
// un-wired seam #357 left at main.js:859), so each row falls back to a PURE-GENERIC sensory phrase
// keyed only on the change kind (added / changed / removed). We NEVER derive a user-visible label
// from the node address: title-casing "/cutoff" → "Cutoff" or "/note-voice" → "Note Voice" just
// leaks the engine/theory vocabulary that address names (§1.2 — filter → "brightness", not
// "cutoff"), and the §1 structural lexicon does not even list the DSP/theory terms the real toys
// use, so a title-cased address would sail past it. `agentCopy` is the LABELED SEAM where the
// agent's own row sentence arrives once the loop lands: when present it renders verbatim (still
// lexicon-gated), otherwise we fall back to the generic phrase. The spec's "Added Shimmer" example
// is agent-supplied copy — NOT something to synthesize from an address — so generic-until-agentCopy
// is the correct §4.5 behavior. Swap the generic phrase for the agent's copy at that seam; the card
// shape does not move. The raw node address still rides in the row's data-node attribute (not
// user-visible) so the surface echo can resolve it to a control.

import { h } from "../dom.js";

// The forbidden engine lexicon (spec §1 / the M1 lexicon gate) the card must NEVER leak to the
// user. Whole-word, case-insensitive. Kept in lockstep with web/tests/spine.spec.js FORBIDDEN and
// the change-card spec's scan — a node address that cleans to one of these is dropped to a neutral
// noun rather than shown. ("plan" is here too: the ENVELOPE field is `plan`, but the WORD must not
// surface as card chrome — so we label no region "plan"; only the agent's sensory plain-language
// plan TEXT renders, which carries none of these words.)
export const FORBIDDEN_LEXICON = [
  "operator", "input", "output", "port", "patch", "wire", "swap", "plan", "address",
  "coordinator", "voicer", "voice", "survivor", "rig", "tuning", "param", "widget", "surface",
];
const FORBIDDEN_RE = FORBIDDEN_LEXICON.map((w) => new RegExp(`\\b${w}\\b`, "i"));

// True if `text` trips the forbidden lexicon (any whole-word hit). Gates the `agentCopy` seam: an
// agent row sentence that somehow carries an engine/theory word is dropped to the generic phrase
// rather than shown. (The synthesized fallback needs no gating — it is generic by construction.)
function tripsLexicon(text) {
  return FORBIDDEN_RE.some((re) => re.test(text));
}

// The one sensory row sentence for a change (spec §4.5). `bucket` is "added" | "changed" |
// "removed"; `nodeAddress` is the diff's node address — kept in the signature because it is the
// row's identity for the surface echo (it rides in data-node), but it is DELIBERATELY never turned
// into user-visible text: an address names engine/theory vocabulary, and the §1 gate does not even
// list the DSP terms the real toys use, so title-casing it would leak "Cutoff" / "Autopan" / "Env
// Vca" straight into a row (findings 1+2). `agentCopy` is the SEAM: the agent's own sensory line
// once its loop is wired, rendered verbatim WHEN it is lexicon-clean. Absent (or unclean) agent
// copy, the row is a PURE-GENERIC sensory phrase keyed only on the change kind — forbidden-word
// clean by construction, and free of any node/operator/theory name.
// eslint-disable-next-line no-unused-vars -- nodeAddress documents the row identity seam (data-node)
export function describeChange(bucket, nodeAddress, agentCopy) {
  if (typeof agentCopy === "string" && agentCopy.trim() && !tripsLexicon(agentCopy)) {
    return agentCopy.trim();
  }
  switch (bucket) {
    case "added":
      return "Added a new layer";
    case "removed":
      return "Removed a layer";
    case "changed":
    default:
      return "Reshaped a control";
  }
}

// Flatten a structural diff (§4.6: {added, changed, removed} of node addresses; `survived` is
// degenerate on web and NEVER read/shown — spec §4.6) into an ordered row list. Order mirrors §4.1's
// "added / changed / removed": additions lead (they pulse in), reshapes next, removals last.
function diffRows(diff) {
  if (!diff) return [];
  const rows = [];
  for (const addr of diff.added ?? []) rows.push({ bucket: "added", addr });
  for (const addr of diff.changed ?? []) rows.push({ bucket: "changed", addr });
  for (const addr of diff.removed ?? []) rows.push({ bucket: "removed", addr });
  return rows;
}

// Big-diff threshold (spec §4.4 / §3.6): enumerate every row for a small reshape; once a change
// crosses ~4–5 changes the card collapses to a summary headline + an expandable "show all." We
// collapse when the total EXCEEDS this — a reshape of exactly this many still enumerates, and a
// 6-change rebuild collapses (the spec's "> 5" verification bar).
export const COLLAPSE_THRESHOLD = 5;

/**
 * Build a change-card bound to one turn envelope (spec §4). Returns a handle whose `el` is a
 * STABLE node the transcript mounts once and reuses; `update()` re-paints its guts from the current
 * envelope (thinking → resolved in place, §4.2). Row hover/focus echo-highlights the control on the
 * `board` (§4.1 linkage); a change with no control (§4.3 no-knob) still gets a row and simply has
 * nothing to echo.
 *
 * @param {import("../../../crates/reuben-web/js/agent-turn.mjs").AgentTurn} turn - the live envelope.
 * @param {{cellsForNode: (a: string) => Element[], echoNode: (a: string) => void,
 *          clearEchoNode: (a: string) => void}} board - the surface board's echo bridge.
 * @param {{onAlternative?: (alt: {id: string, label: string}) => void}} [opts] - case 1's
 *   alternative-interpretation chip tap (spec §5.1): a tap re-reshapes toward the other reading.
 *   Wired by the spine to `submitTurn(alt.label)` — the same verbatim-post path a typed line takes.
 * @returns {{el: HTMLElement, turn: object, update: () => void}}
 */
export function createChangeCard(turn, board, { onAlternative } = {}) {
  // The persistent card node (spec §4.2 "the same object"). `data-card-state` mirrors the envelope
  // status so the spine + tests can observe thinking → resolved on ONE element.
  const el = h("div", {
    class: "tx-card",
    dataset: { turnId: turn.id, cardState: turn.status },
    role: "group",
  });
  // §4.4's "show all" is a persistent per-card toggle: once the reader expands a big diff it stays
  // open across re-renders (a plan delta must not re-collapse it). Held here, outside render().
  let showAll = false;

  // Wire a row's echo linkage (spec §4.1): hover/focus glows its control on the surface, leaving
  // clears it — the surface glow is transient, the card row persists. Pointer AND focus so it works
  // for touch/keyboard, not mouse only. A no-knob row (§4.3) wires the same handlers; they no-op
  // because the board has no cell for that node.
  function wireEcho(rowEl, addr) {
    if (!addr) return;
    const on = () => board.echoNode(addr);
    const off = () => board.clearEchoNode(addr);
    rowEl.addEventListener("mouseenter", on);
    rowEl.addEventListener("mouseleave", off);
    rowEl.addEventListener("focus", on);
    rowEl.addEventListener("blur", off);
  }

  function rowEl({ bucket, addr }) {
    const text = describeChange(bucket, addr, /* agentCopy seam: */ undefined);
    // A row is echo-able only when the node backs a control (§4.3). data-knob lets the surface-echo
    // test assert "a no-knob change fires no surface echo" without reaching into the board.
    const hasControl = (board.cellsForNode(addr) ?? []).length > 0;
    const row = h(
      "li",
      {
        class: "tx-card-row",
        tabindex: "0",
        dataset: { bucket, node: addr, knob: String(hasControl) },
      },
      h("span", { class: "tx-card-row-mark", "aria-hidden": "true" }, MARK[bucket] ?? ""),
      h("span", { class: "tx-card-row-text" }, text),
    );
    wireEcho(row, addr);
    return row;
  }

  // Repaint the card's guts from the current envelope. Idempotent; called on every plan delta and
  // on resolve. NEVER replaces `el` — that is the resolve-in-place guarantee (§4.2).
  function render() {
    el.dataset.cardState = turn.status;
    el.replaceChildren();

    // The authoring voice's stamp — consistent with the transcript's other lines.
    el.appendChild(h("span", { class: "tx-role" }, "reuben"));

    // The plan (spec §4.2): the streamed natural-language intent, shown AS it grows and kept as the
    // lead summary once resolved. It is the agent's own sensory prose (lexicon-clean by authoring),
    // so it renders verbatim. Empty until the first token arrives.
    const plan = h("p", { class: "tx-card-plan" }, turn.plan || "");
    el.appendChild(plan);

    if (turn.status === "resolved") {
      const rows = diffRows(turn.diff);
      if (rows.length > COLLAPSE_THRESHOLD) {
        // §4.4 big diff: a summary headline + an expandable "show all changes". The enumerated rows
        // still render (so the reader CAN scan them) but stay hidden until expanded.
        el.appendChild(h("p", { class: "tx-card-headline" }, "Rebuilt — essentially a new sound"));
        el.appendChild(
          h(
            "button",
            {
              class: "tx-card-showall",
              type: "button",
              "aria-expanded": String(showAll),
              onclick: () => {
                showAll = !showAll;
                render();
              },
            },
            showAll ? "Hide changes" : `Show all ${rows.length} changes`,
          ),
        );
        el.appendChild(
          h(
            "ul",
            { class: "tx-card-rows", dataset: { expanded: String(showAll) } },
            rows.map(rowEl),
          ),
        );
      } else if (rows.length > 0) {
        // Small reshape: enumerate every change (spec §4.4).
        el.appendChild(h("ul", { class: "tx-card-rows", dataset: { expanded: "true" } }, rows.map(rowEl)));
      }
      // rows.length === 0 → a resolved turn that touched no node (e.g. a pure re-audition): the plan
      // line alone is the record. No empty rows list.

      // §5.1 case 1 — ambiguous but actionable: the best-effort change already played (the rows +
      // surface glow above); here we surface HOW the guess was read + the tappable other-readings.
      // Both ride this card (spec §5.2: a change happened, so it is a change-card, not a chat turn).
      // The reading line is the agent's own sensory prose — lexicon-gated like `agentCopy`, dropped
      // if it somehow carries an engine word rather than shown unclean.
      if (typeof turn.reading === "string" && turn.reading.trim() && !tripsLexicon(turn.reading)) {
        el.appendChild(h("p", { class: "tx-card-reading" }, turn.reading.trim()));
      }
      // The alternative-interpretation chips (spec §5.1): 1-2 tappable other-readings. Each posts its
      // label VERBATIM as the next turn (the §2.3 chip contract) — a wrong guess is one tap from
      // fixed, no typing. A label that trips the lexicon is dropped, never shown unclean.
      const alts = (turn.alternatives ?? []).filter(
        (a) => a && typeof a.label === "string" && a.label.trim() && !tripsLexicon(a.label),
      );
      if (alts.length > 0) {
        el.appendChild(
          h(
            "div",
            { class: "tx-card-alts tx-chips" },
            alts.map((alt) =>
              h(
                "button",
                {
                  class: "tx-chip tx-alt-chip",
                  type: "button",
                  dataset: { altId: alt.id ?? "" },
                  onclick: () => onAlternative?.(alt),
                },
                alt.label.trim(),
              ),
            ),
          ),
        );
      }
    }

    // §4.7: the restart-honesty slot is RESERVED at the card foot and rendered only if the envelope
    // carries it. #360 (the re-strike) owns filling + animating it; this ticket fixes only WHERE it
    // lands. Kept present-but-empty otherwise (CSS :empty hides it) so #360 has a named home.
    const honesty = h("p", { class: "tx-card-honesty", dataset: { slot: "restart-honesty" } });
    if (typeof turn.restartHonesty === "string" && turn.restartHonesty) {
      honesty.textContent = turn.restartHonesty;
    }
    el.appendChild(honesty);
  }

  render();
  return { el, turn, update: render };
}

// The per-bucket row glyph (sensory, not a word — the text carries the meaning, this is a quiet
// scan cue): a plus for an addition, a swap-free tilde for a reshape, a minus for a removal.
const MARK = { added: "+", changed: "~", removed: "−" };

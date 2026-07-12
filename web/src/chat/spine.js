// chat/spine.js — the co-presence spine (spec §3, issue #355): the KEYSTONE layout every other
// M1 chat UI ticket renders on. It inverts the Canvas/Artifacts relationship (spec §3.2): the
// running instrument's surface is the always-on PRIMARY artifact; chat is the EDITOR bound to
// it. One responsive layout, phone → desktop (spec §3.7):
//
//   - the surface board owns the TOP / CENTER at every width (chat/board.js);
//   - the reshape input line is PINNED TO THE BOTTOM (thumb-reachable on a phone, fine on
//     desktop) and is ALWAYS visible — never behind a menu (spec §3.3);
//   - the transcript is an UPWARD collapsible sheet that partially occludes the LOWER surface
//     when open — never a full-screen takeover (that is the ruled-out mode-switch, spec §3.1).
//
// Modeless + co-presence (spec §3.1 / §3.4): there is NO play/reshape mode switch and the
// controls stay hand-live THROUGH an agent turn — `setTurnInFlight` toggles a state attribute
// for the chrome to reflect but NEVER disables the board or the input, so a reshape happens in
// view of, and without freezing, the running instrument. The ONLY interruption is the structural
// restart at swap-time — that moment is the re-strike ticket's (#360); `onStructuralRestart` is
// the clean no-op seam it lands in, wired to nothing here.
//
// SCOPE: this ticket builds the spine and its seams ONLY. The real agent loop (#354), the
// change-card + surface highlight (#358), the cold-start gallery + chips (#357) and the Keep
// control (M2) each own their piece — the spine renders against a MINIMAL local transcript model
// (chat/transcript.js) and a MOCK turn so none of those need to exist yet. The bottom-chrome
// Keep slot is left as an empty, labeled container (M2 fills it).

import "./spine.css";
import { h } from "../dom.js";
import { createBoard } from "./board.js";
import { createTranscript } from "./transcript.js";

// The arrival-default transcript state (spec §3.3): land EXPANDED when the user MADE an
// instrument (they were just talking), COLLAPSED-TO-BAR when they PICKED one to play. Exposed as
// a builder parameter so the cold-start / gallery ticket (#357) passes the right value per path;
// this ticket just honors it.
const ARRIVAL_EXPANDED = { made: true, picked: false };

/**
 * Build the co-presence spine (spec §3).
 *
 * @param {object} [opts]
 * @param {"made"|"picked"} [opts.arrival] - arrival default (spec §3.3): "made" lands expanded,
 *   "picked" lands collapsed-to-bar. Defaults to "picked".
 * @param {(text: string, api: object) => void} [opts.onReshapeSubmit] - handler for a submitted
 *   reshape line. Defaults to the MOCK turn (proves controls stay live); #354 overrides it with
 *   the real agent loop, pushing its streamed plan / resolved card onto `transcript`.
 * @param {Array<{role: string, text?: string, kind?: string, chips?: string[]}>} [opts.seed] -
 *   the turn-one content the caller already knows (spec §2.3/§2.4), pushed into the transcript at
 *   creation — the cold-start / gallery ticket (#357)'s proactive greeting (+ authored chips) for
 *   a pick, or the user's own words echoed for a describe-path build. Empty by default (a caller
 *   that wants no seed content, or that pushes its own turn one asynchronously via `transcript`).
 * @returns {object} the spine handle (screen + board + transcript + turn/sheet controls + seams).
 */
export function createSpine({ arrival = "picked", onReshapeSubmit, seed = [] } = {}) {
  const expanded = ARRIVAL_EXPANDED[arrival] ?? false;

  const board = createBoard();
  const transcript = createTranscript();

  // --- transcript sheet (spec §3.3): the UPWARD collapsible conversation ---------------------
  // A `kind: "chips"` entry (spec §2.3/§I) renders as a row of tappable quick-change chips
  // instead of a text bubble. Each chip posts its string VERBATIM through the SAME `submitTurn`
  // path a typed-and-sent line takes — "what you said is what happened" (spec §2.3): a chip tap
  // and re-typing the identical phrase are indistinguishable to the turn loop.
  const transcriptView = h("div", { class: "transcript", role: "log", "aria-live": "polite" });
  const renderTranscript = () => {
    transcriptView.replaceChildren(
      ...transcript.entries.map((e) => {
        if (e.kind === "chips") {
          return h(
            "div",
            { class: "tx-entry tx-chips-entry", dataset: { role: e.role } },
            h(
              "div",
              { class: "tx-chips" },
              (e.chips ?? []).map((chip) =>
                h(
                  "button",
                  { class: "tx-chip", type: "button", onclick: () => submitTurn(chip) },
                  chip,
                ),
              ),
            ),
          );
        }
        return h(
          "div",
          { class: "tx-entry", dataset: { role: e.role } },
          h("span", { class: "tx-role" }, e.role === "you" ? "You" : "reuben"),
          h("p", { class: "tx-text" }, e.text),
        );
      }),
    );
    // Newest line into view when the sheet is open.
    transcriptView.scrollTop = transcriptView.scrollHeight;
  };
  transcript.subscribe(renderTranscript);

  const handleLabel = h("span", { class: "sheet-handle-label" }, "Conversation");
  const handle = h(
    "button",
    {
      class: "sheet-handle",
      type: "button",
      "aria-expanded": String(expanded),
      onclick: () => toggleSheet(),
    },
    h("span", { class: "sheet-chevron", "aria-hidden": "true" }, "▾"),
    handleLabel,
  );
  const sheet = h(
    "div",
    { class: "spine-sheet", dataset: { expanded: String(expanded) } },
    handle,
    transcriptView,
  );

  // --- bottom chrome (spec §3.3 + §7.3): pinned input + the empty Keep slot (M2) -------------
  const input = h("input", {
    class: "reshape-input",
    type: "text",
    name: "reshape",
    autocomplete: "off",
    // Sensory-first placeholder (spec §1); mirrors the "describe your own" cold-start bar (§2.1).
    placeholder: "Describe a change… (e.g. make it brighter)",
    "aria-label": "Describe a change to your instrument",
  });
  const sendBtn = h("button", { class: "reshape-send", type: "submit" }, "Send");
  const form = h(
    "form",
    {
      class: "reshape",
      onsubmit: (ev) => {
        ev.preventDefault();
        const text = input.value.trim();
        if (!text) return; // empty send is the failure ticket's gentle re-orient (#303/E), not ours
        input.value = "";
        submitTurn(text);
      },
    },
    input,
    sendBtn,
  );

  // The bottom-chrome slot the Keep control (M2, spec §7.3) mounts into — left EMPTY here, labeled
  // only by `data-slot`, so the Keep ticket has a named, thumb-reachable home by the input line
  // without this ticket building (or naming) Keep. Do NOT put a control here.
  const keepSlot = h("div", { class: "keep-slot", dataset: { slot: "keep" } });

  const chrome = h("div", { class: "spine-chrome" }, keepSlot, form);
  const dock = h("div", { class: "spine-dock" }, sheet, chrome);

  // --- the surface region (spec §3.7): owns TOP / CENTER at every width -----------------------
  const surfaceRegion = h("div", { class: "spine-surface" }, board.el);

  const screen = h(
    "section",
    {
      class: "spine",
      dataset: { arrival, turn: "idle", sheet: String(expanded) },
    },
    surfaceRegion,
    dock,
  );

  // --- sheet + turn state --------------------------------------------------------------------
  function toggleSheet(force) {
    const next = typeof force === "boolean" ? force : sheet.dataset.expanded !== "true";
    sheet.dataset.expanded = String(next);
    screen.dataset.sheet = String(next);
    handle.setAttribute("aria-expanded", String(next));
    if (next) transcriptView.scrollTop = transcriptView.scrollHeight;
  }

  // Toggle the turn-in-flight STATE ONLY (spec §3.4). Co-presence guarantee: this reflects the
  // turn in the chrome but NEVER disables the board or the input — the instrument keeps playing
  // and keeps responding to the user's hands while the agent works. The only thing that may
  // interrupt is the structural restart (#360), via `onStructuralRestart`, not this.
  function setTurnInFlight(flag) {
    screen.dataset.turn = flag ? "in-flight" : "idle";
  }

  // The MOCK turn (#354 seam): push the user's line, mark the turn in flight, then settle. It
  // deliberately changes NOTHING on the surface and mints no fake reply — it exists only to prove
  // the controls stay live across a turn. #354 replaces this with the real agent loop.
  function mockReshape(text, self) {
    self.beginMockTurn(text);
    setTimeout(() => self.endMockTurn(), 700);
  }

  // The ONE submit path every source of a turn goes through — the pinned input's Send AND a
  // tappable chip (spec §2.3's "what you said is what happened": a chip is not a separate,
  // hidden action, it is the SAME turn a typed-and-sent line would produce).
  function submitTurn(text) {
    if (!text) return;
    (onReshapeSubmit ?? mockReshape)(text, api);
  }

  const api = {
    screen,
    board,
    transcript,
    keepSlot,

    // Turn lifecycle (mock; #354 owns the real one).
    setTurnInFlight,
    beginMockTurn(text) {
      if (text) transcript.push({ role: "you", text });
      setTurnInFlight(true);
    },
    endMockTurn(reply) {
      if (reply) transcript.push({ role: "reuben", text: reply });
      setTurnInFlight(false);
    },
    turnInFlight: () => screen.dataset.turn === "in-flight",

    // Sheet.
    toggleSheet,
    sheetExpanded: () => sheet.dataset.expanded === "true",

    // §3.6 re-layout seams live on the board; re-export for callers that hold the spine.
    relayout: board.relayout,
    onSuggestRelayout: board.onSuggestRelayout,

    // #360 seam — the structural restart / re-strike moment (the declicked duck + replay-from-top,
    // spec §6). The ONE interruption the spine allows (spec §3.4). A no-op here: assign a real
    // implementation from the re-strike ticket; the spine never calls it itself.
    onStructuralRestart() {
      /* no-op seam (#360) */
    },
  };

  // Seed the caller's turn-one content (spec §2.3/§2.4 — the cold-start / gallery ticket, #357:
  // a pick's greeting + authored chips, or a describe-path's echoed first message) and paint the
  // transcript. Empty by default — a caller with nothing to seed synchronously just gets a blank
  // transcript and pushes its own content onto `transcript` once it's ready.
  for (const entry of seed) transcript.push(entry);
  renderTranscript();

  return api;
}

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
import { createChangeCard } from "./change-card.js";
import { assistantTurn } from "../../../crates/reuben-web/js/agent-turn.mjs";

// The arrival-default transcript state (spec §3.3): land EXPANDED when the user MADE an
// instrument (they were just talking), COLLAPSED-TO-BAR when they PICKED one to play. Exposed as
// a builder parameter so the cold-start / gallery ticket (#357) passes the right value per path;
// this ticket just honors it.
const ARRIVAL_EXPANDED = { made: true, picked: false };

// The re-strike's REPLAY-FROM-TOP cue (spec §6.2.2, issue #360): a slim transport strip above the
// board whose playhead sweeps left→right and, on a structural restart, VISIBLY RETURNS TO THE
// START. That return is the load-bearing honesty: a structural swap resets every node cold and the
// clock/sequencer to step 0 (ADR-0046 §10), so snapping the playhead to the start at the re-strike
// moment SHOWS the phase reset the spec calls the salient event — "playing the new version from the
// beginning." The continuous sweep between re-strikes is a v1 VISUAL cue, not a sample-accurate
// playhead: M1 exposes no clock/transport position to the shell, so binding the sweep to the real
// loop phase is a named deferred door (alongside quantize-to-downbeat, §6.3). What IS honest today
// is the reset — driven only by a genuine re-strike, never faked.
function createTransport() {
  const playhead = h("div", { class: "transport-playhead", "aria-hidden": "true" });
  const el = h(
    "div",
    { class: "spine-transport", dataset: { restrikeSeq: "0" }, role: "presentation" },
    playhead,
  );
  let seq = 0;
  return {
    el,
    // Snap the playhead back to the start and re-run the sweep from step 0 (spec §6.2.2). Re-trigger
    // the CSS animation by clearing it, forcing a reflow, then restoring it — the reliable
    // restart-an-animation idiom. `data-restrike-seq` is a monotonic stamp the re-strike spec reads
    // to prove the reset FIRED (and fired exactly once per structural change), race-free.
    restrike() {
      seq += 1;
      el.dataset.restrikeSeq = String(seq);
      playhead.style.animation = "none";
      void playhead.offsetWidth; // force reflow so the animation genuinely restarts from 0
      playhead.style.animation = "";
    },
    restrikeSeq: () => seq,
  };
}

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
 * @param {(atSilence: () => (void|Promise<void>)) => Promise<void>} [opts.duck] - the re-strike's
 *   declicked audio duck (spec §6.2.4, issue #360): fade to silence, run `atSilence` at the trough,
 *   fade back up. main.js wires the live engine's `restrikeDuck` here so the visible commit is
 *   co-timed with the real sound drop. Defaults to an IMMEDIATE pass-through (runs `atSilence` now,
 *   no audio) so the visible re-strike gesture works headless / before an engine is bound.
 * @returns {object} the spine handle (screen + board + transcript + turn/sheet controls + seams).
 */
export function createSpine({ arrival = "picked", onReshapeSubmit, seed = [], duck } = {}) {
  const expanded = ARRIVAL_EXPANDED[arrival] ?? false;
  // The declicked duck (spec §6.2.4). Absent a wired engine, a synchronous pass-through: the trough
  // work still runs (co-timed commit stays honest), just with no audible fade.
  const runDuck =
    duck ??
    ((atSilence) => Promise.resolve(atSilence?.()));

  const board = createBoard();
  const transcript = createTranscript();
  const transport = createTransport();

  // --- transcript sheet (spec §3.3): the UPWARD collapsible conversation ---------------------
  // A `kind: "chips"` entry (spec §2.3/§I) renders as a row of tappable quick-change chips
  // instead of a text bubble. Each chip posts its string VERBATIM through the SAME `submitTurn`
  // path a typed-and-sent line takes — "what you said is what happened" (spec §2.3): a chip tap
  // and re-typing the identical phrase are indistinguishable to the turn loop.
  const transcriptView = h("div", { class: "transcript", role: "log", "aria-live": "polite" });
  const renderTranscript = () => {
    transcriptView.replaceChildren(
      ...transcript.entries.map((e) => {
        // A `kind: "change-card"` entry (spec §4, issue #358) carries a STATEFUL card component
        // whose `.el` is a stable node — return that SAME element every render so its thinking →
        // resolved repaint lands in place (§4.2), not as a re-created (or duplicated) card.
        if (e.kind === "change-card") return e.card.el;
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
  // A named, empty mount ABOVE the board for the Enable-microphone affordance (#248/#357): a
  // live-input Toy (Mic Space) is SILENT until the user grants the mic on a gesture, so its pick
  // must surface an enable control here — mounted by the caller (main.js `openSpine`) keyed on the
  // instrument's inputChannels, exactly as the player does. Stays EMPTY for self-playing /
  // tap-to-play Toys. Kept a bare container (like `keepSlot`) so the spine owns no mic logic.
  const micSlot = h("div", { class: "spine-mic", dataset: { slot: "mic" } });
  // The re-strike transport (spec §6.2.2, issue #360) sits between the mic affordance and the board:
  // the replay-from-top cue reads at the surface's head, above the controls it replays.
  const surfaceRegion = h("div", { class: "spine-surface" }, micSlot, transport.el, board.el);

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
    transport,
    keepSlot,
    micSlot,

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

    // --- the change-card + surface highlights (spec §4, #358) + the re-strike (spec §6, #360) --
    // Open a change-card for a reshape (spec §4.1/§4.2). Builds a turn envelope in the "thinking"
    // state (js/agent-turn.mjs — THE #354 contract), mounts its card into the transcript IMMEDIATELY
    // (so the plan can start streaming at once), and returns a controller the caller drives:
    //   - `appendPlan(text)`  grow the streamed plan; the card repaints in place (§4.2).
    //   - `resolve(diff)` — the §6.1 PARAMETER-ONLY path: transition thinking → resolved on the SAME
    //     card into sensory rows + the diff-keyed surface glow (§4.1/§4.6), a live sweep with no gap.
    //   - `restrike(diff, honesty, {sounding})` — the §6.2 STRUCTURAL path: the declicked-duck
    //     re-strike, committing the card + surface + playhead reset at the trough, with the
    //     first-run-only restart line (§6.4) rendered from the envelope.
    // The routing between the two is the agent's (#356); this renders each behavior. The real agent
    // loop (#354) drives this exactly as the tests do; until it is wired into the browser this is the
    // seam a crafted envelope pushes through (mirrors main.js:859's un-wired seam).
    beginReshapeCard() {
      const env = assistantTurn();
      const card = createChangeCard(env.turn, board);
      transcript.push({ kind: "change-card", card });

      // The card-commit + surface-animate half of a landed reshape (spec §4.1/§4.6). Factored out so
      // BOTH resolve paths below share it: the param-only live sweep runs it immediately, the
      // structural re-strike runs it AT the duck's trough (co-timed with the sound drop, §6.2.1).
      //
      // GLOW ONLY (finding 3 / §4.1 "a changed control sweeps its value"): this fires the
      // node-identity GLOW, not the value re-render — we do not call `board.update(...)`. The
      // structural diff carries node addresses, not the reshaped widget's new value, so there is
      // nothing to sweep the control TO yet. The value-sweep half of §4.1 lands with the #354
      // agent-loop wiring, which delivers the fresh widget set alongside the diff (the seam at
      // main.js:859): at that point this also re-renders the touched controls through `board.update`
      // and the "changed" glow doubles as the value-sweep. Until then it is honestly glow-only.
      function commit() {
        env.resolve();
        card.update();
        board.highlightDiff(env.turn.diff);
      }

      return {
        turn: env.turn,
        appendPlan(text) {
          env.appendPlan(text);
          card.update();
        },

        // §6.1 PARAMETER-ONLY path: a live sweep, NO gap, no phase reset — the existing #358 behavior.
        // The routing (param-only → `send`) is the agent's (#356); this renders it as the
        // magnitude-appropriate live control move. A param-only reshape NEVER carries a restart line
        // (spec §6.4: nothing restarted) — `honesty` is deliberately not accepted here.
        resolve(diff) {
          if (diff) env.setDiff(diff);
          commit();
          return env.turn;
        },

        // §6.2 STRUCTURAL path: THE re-strike. A structural reshape (add/remove/rewire) restarts the
        // sound cold (ADR-0046 §10), so it is presented as "the instrument replaying with your
        // change, from the top" — never named as a restart to the user (§6.1). The gesture:
        //   1. co-timed cause (§6.2.1) — the card commits + the surface animates AT the sound drop,
        //      inside the duck's trough (`commit` runs there), so cause and effect are simultaneous;
        //   2. replay-from-top (§6.2.2) — the transport playhead visibly returns to the start;
        //   3. a decisive, consistent gesture with NO spinner over the gap (§6.2.3) — it's a beat,
        //      not a wait; we add no loading chrome, and the turn-in-flight stripe is not touched;
        //   4. a clean declicked duck (§6.2.4) — `runDuck` fades to silence and back (raised-cosine,
        //      js/declick.mjs), never a hard cut.
        // `sounding` gates §6.4's "nothing currently playing → just build ready": with no live sound
        // there is no restart to be honest about, so we skip the duck, the playhead reset, AND the
        // honesty line, and simply commit (build-and-be-ready). `honesty` renders the first-run-only
        // restart line the ENVELOPE already gates once/session (#356 attaches it; we only render the
        // slot #358 reserved — never re-implement the gate). Resolves when the duck completes.
        async restrike(diff, honesty, { sounding = true } = {}) {
          if (diff) env.setDiff(diff);
          if (!sounding) {
            commit(); // build-and-be-ready: no duck, no playhead reset, no restart line (§6.4)
            return env.turn;
          }
          await runDuck(() => {
            if (typeof honesty === "string" && honesty) env.turn.restartHonesty = honesty;
            commit(); // the co-timed cause: card + surface land exactly as the sound drops (§6.2.1)
            transport.restrike(); // replay-from-top: the playhead visibly returns to start (§6.2.2)
          });
          return env.turn;
        },
      };
    },

    // Sheet.
    toggleSheet,
    sheetExpanded: () => sheet.dataset.expanded === "true",

    // §3.6 re-layout seams live on the board; re-export for callers that hold the spine.
    relayout: board.relayout,
    onSuggestRelayout: board.onSuggestRelayout,

    // #360 — the structural restart / re-strike moment (spec §6) is now IMPLEMENTED on the
    // reshape-card controller: `beginReshapeCard().restrike(diff, honesty, {sounding})` runs the
    // declicked duck (§6.2.4) with the co-timed card-commit + surface-animate + replay-from-top at
    // its trough (§6.2.1/§6.2.2). This stays as the named seam the spec §3.4 note points at — the
    // spine still never triggers a restart on its own; a restart only ever happens through a
    // structural `restrike` a turn drives. Left as a no-op so nothing here fires audio unbidden.
    onStructuralRestart() {
      /* implemented via beginReshapeCard().restrike (#360); this seam stays a no-op */
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

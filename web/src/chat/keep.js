// chat/keep.js — THE KEEP GESTURE (spec §7, issue #359): the ADR-0042 share-link mechanism
// re-presented as a SAVE, and the ADR-0052 §3 ship gate — the chat window ships once this is
// wired into the spine's loop, and not before (in-page `swap` is by-value; page memory is
// volatile). This module owns the Keep control's PRESENTATION + its ephemerality STATE MACHINE;
// the actual snapshot write / clipboard copy is main.js's (it holds the engine + codec path) and
// arrives as the `onKeep` callback — the browser needs a user gesture for the clipboard, so the
// mint happens on the user's tap here, never from the agent (spec §7.8).
//
// The control lives in the spine's bottom chrome, by the pinned input (spec §7.3) — always visible
// whether the transcript sheet is expanded or collapsed, thumb-reachable. It carries a persistent
// state that teaches volatility passively (spec §7.4): "Not kept yet" → "Kept ✓". NOT "Save"
// (over-promises a cloud/account that doesn't exist — §1's honesty rule) and NOT "Share"
// (foregrounds an audience the first-timer may not have); the link is your save, sharing rides
// along on the same link (spec §7.1).
//
// THREE state axes, tracked here so main.js wires the leave-guard to one predicate:
//   - kept:      has the CURRENT live instrument been written to the hash? Flips true on a keep,
//                back to false on a later diverging reshape (staleness, the unsaved-changes model,
//                spec §7.7).
//   - diverged:  has the instrument left its re-findable origin? A gallery pick the user hasn't
//                touched is re-findable (still in the gallery) → NOT diverged → not at risk (spec
//                §7.4). A described-own build or a reshape diverges it. Monotonic — once diverged,
//                staying diverged (the underlying instrument is the user's now).
//   - leaveGuardArmed = diverged && !kept: the ONLY condition a navigate-away leave-guard fires on
//                (spec §7.4). main.js reads this in its one beforeunload handler.
//
// PROACTIVITY (spec §7.5): the FIRST divergence announces itself with a single subtle pulse on the
// chip, then sits quiet — proactive exactly once, on the chrome, never a chat nag. A later
// diverging reshape re-arms the guard but does NOT re-pulse (the one-time pulse already fired). The
// agent pointing to Keep (spec §7.8, `pointToKeep`) is a SEPARATE pulse trigger — it fires each
// time the user asks about saving, and directs the eye without the once-guard.

import { h } from "../dom.js";

// The non-modal confirm shown after a successful keep (spec §7.6). Leads with keep-to-not-lose
// (bookmark to come back), sharing rides along (spec §7.1) — forbidden-word-clean (spec §1).
const CONFIRM_COPIED = "Kept ✓ — bookmark this page to come back, or paste the link anywhere to share.";
// Same, when the clipboard was unavailable so the link couldn't be auto-copied: the hand-copy field
// below carries the link instead. Still keep-to-not-lose first.
const CONFIRM_NO_CLIPBOARD = "Kept ✓ — bookmark this page to come back. Copy the link below to share.";

/**
 * Build the Keep control + its ephemerality state machine (spec §7).
 *
 * @param {object} opts
 * @param {() => Promise<{url: string, copied: boolean} | null>} opts.onKeep - performs the actual
 *   snapshot write to `location.hash` + the clipboard copy (main.js, reusing the ADR-0042 codec),
 *   invoked on the user's tap so the clipboard write rides a real gesture (spec §7.8). Returns the
 *   minted URL + whether the copy landed, or null when there is nothing to keep.
 * @returns {object} the keep handle (`el` + state machine + test-observable getters).
 */
export function createKeep({ onKeep }) {
  let kept = false;
  let diverged = false;
  let pulsedOnce = false; // the first-divergence one-time pulse (spec §7.5) has fired
  let pulseCount = 0; // TEST-ONLY: every pulse (first-divergence auto + agent-directed) counted

  const state = h("span", { class: "keep-state" }, "Not kept yet");
  const btn = h(
    "button",
    {
      class: "keep-btn",
      type: "button",
      // The word is "Keep" (spec §7.2) — the accessible name of the action; the visible text is the
      // persistent state it reports ("Not kept yet" → "Kept ✓"), so the chip teaches volatility.
      "aria-label": "Keep",
      title: "Keep — your link is your save",
    },
    h("span", { class: "keep-mark", "aria-hidden": "true" }, "🔖"),
    state,
  );
  // A brief NON-MODAL confirm (spec §7.6): a popover above the chip, auto-dismissed. `aria-live`
  // announces it without stealing focus. Holds the hand-copy field on a no-clipboard fallback.
  const confirm = h("div", { class: "keep-confirm", role: "status", "aria-live": "polite", hidden: "" });
  const el = h(
    "div",
    { class: "keep-control", dataset: { kept: "false", diverged: "false" } },
    confirm,
    btn,
  );

  let confirmTimer = null;

  function render() {
    el.dataset.kept = String(kept);
    el.dataset.diverged = String(diverged);
    state.textContent = kept ? "Kept ✓" : "Not kept yet";
  }

  // Trigger the one-shot pulse animation (spec §7.5 / §4's surface-highlight visual language). The
  // clear + reflow + set idiom lets it re-fire (the agent can point more than once, spec §7.8).
  function pulse() {
    pulseCount += 1;
    el.dataset.pulse = "off";
    void el.offsetWidth; // force reflow so the animation genuinely restarts
    el.dataset.pulse = "on";
  }

  function showConfirm(text, url, copied) {
    confirm.replaceChildren(h("p", { class: "keep-confirm-text" }, text));
    // No-clipboard fallback: a selected read-only field so the link is copyable by hand (parity
    // with the player's share fallback). The engine's clipboard write already ran on the tap.
    if (!copied && url) {
      const field = h("textarea", { class: "keep-copy-field", readonly: "", rows: "2" });
      field.value = url;
      confirm.append(field);
      confirm.hidden = false;
      field.focus();
      field.select();
    } else {
      confirm.hidden = false;
    }
    if (confirmTimer) clearTimeout(confirmTimer);
    // A copied link self-dismisses; a hand-copy field lingers so the user can grab it.
    if (copied) confirmTimer = setTimeout(() => { confirm.hidden = true; }, 4000);
  }

  // The keep action — the user's tap. Mints + persists + copies via `onKeep` (main.js), then flips
  // to "Kept ✓" and shows the confirm. A no-op if there's nothing to keep (onKeep → null).
  async function keep() {
    btn.disabled = true;
    try {
      const result = await onKeep();
      if (!result) return;
      kept = true;
      render();
      showConfirm(result.copied ? CONFIRM_COPIED : CONFIRM_NO_CLIPBOARD, result.url, result.copied);
    } catch (err) {
      // Keep is always available in v1 (sample-free lane, spec §7.9), so a mint failure is not
      // expected — degrade to a visible, lexicon-clean note rather than a silent throw.
      confirm.replaceChildren(
        h("p", { class: "keep-confirm-text" }, `Couldn't make a link — ${err?.message || err}.`),
      );
      confirm.hidden = false;
    } finally {
      btn.disabled = false;
    }
  }
  btn.addEventListener("click", keep);

  return {
    el,

    // The instrument DIVERGED (a described-own build, or a reshape landed — spec §7.4/§7.7). Sets
    // diverged (monotonic), makes any prior keep STALE (kept → false, re-arming the leave-guard),
    // and pulses ONCE on the very first divergence (spec §7.5 — no re-pulse on later reshapes).
    markDiverged() {
      const firstDivergence = !diverged;
      diverged = true;
      kept = false; // a diverging reshape supersedes the last keep (unsaved-changes model, §7.7)
      render();
      if (firstDivergence && !pulsedOnce) {
        pulsedOnce = true;
        pulse();
      }
    },

    // The agent POINTS to Keep (spec §7.8): reuben answers a save/share question in chat and pulses
    // the control so the eye goes to it — the mint still happens on the user's tap, never here. No
    // once-guard: the agent may direct the eye each time the user asks. Mints nothing, writes no hash.
    pointToKeep() {
      pulse();
    },

    // The leave-guard predicate (spec §7.4): fire ONLY on diverged, un-re-findable, unkept work. An
    // untouched gallery pick is re-findable (never diverged) → false. main.js's one beforeunload
    // handler reads this.
    leaveGuardArmed: () => diverged && !kept,

    // TEST-observable state.
    isKept: () => kept,
    isDiverged: () => diverged,
    pulseCount: () => pulseCount,

    // Programmatic keep (the button's click handler; exposed so a test can drive it without a
    // synthetic DOM click and await the async mint).
    keep,
  };
}

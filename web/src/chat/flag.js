// chat/flag.js — the SINGLE source-of-truth ship gate for the in-browser chat authoring
// experience (epic #350 M1). OFF by default: the chat window must NOT reach normal users before
// M2's Keep gesture is wired into its loop. The GATE CRITERION is acceptance criterion §9(7)
// ("the chat window ships only once Keep is wired in") + ADR-0052 §3's by-value / ordering
// constraint on ship; spec §7 is the Keep section that explains WHY Keep must land first, not the
// gate itself. Every M1 chat UI ticket imports THIS predicate — there is exactly one place the
// gate lives, so no screen can drift out of sync with the others.
//
// Two ways to opt in, checked at call time (never cached — the query string can change under a
// SPA hashchange, and a test may navigate):
//   - `?chat=1` (or any presence of a `chat` query param) — how Playwright specs and a hands-on
//     dev flip it on for one page load without a rebuild;
//   - `VITE_REUBEN_CHAT=1` at build time — how a preview deploy can bake the experience on.
//
// When this returns false the existing splash → launcher → player flow (main.js) runs
// byte-for-byte unchanged; when true, the boot routes into the co-presence spine (spec §3).

/**
 * Whether the in-browser chat authoring experience is enabled for this page load.
 *
 * @returns {boolean}
 */
export function chatEnabled() {
  // location may be absent in a non-browser import (defensive; the app only calls this in-page).
  const search = typeof location !== "undefined" ? location.search : "";
  if (new URLSearchParams(search).has("chat")) return true;
  // import.meta.env is Vite's build-time env; the var is a string "1" when baked on.
  return import.meta.env?.VITE_REUBEN_CHAT === "1";
}

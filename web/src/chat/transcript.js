// chat/transcript.js — the conversation model the spine renders (spec §3.3). A DELIBERATELY
// minimal local interface, NOT the real agent host: the spine ticket (#355) must stand on its
// own without importing #354's agent loop or #358's change-card, so it defines the smallest
// model those tickets can later wire into — an ordered list of entries you push to, plus a
// subscribe hook the view re-renders from.
//
// An entry is `{ role, text, kind }`:
//   - role: "you" (the user's words) or "reuben" (the authoring voice). Rendered as the speaker.
//   - text: the sensory-only line (spec §1 lexicon gate applies to everything user-visible here).
//   - kind: "message" today. #358's change-card lands as a richer entry kind rendered by the
//     transcript view — the seam is the open `kind` field + the entry object, nothing more.
//
// No engine, no network, no tool layer. #354 replaces the mock turn in spine.js with the real
// loop and pushes its streamed plan / resolved card as entries here; this model does not change.

/**
 * Create an empty transcript model.
 *
 * @returns {{
 *   entries: Array<{role: string, text: string, kind: string}>,
 *   push: (entry: {role: string, text: string, kind?: string}) => object,
 *   subscribe: (fn: () => void) => (() => void),
 * }}
 */
export function createTranscript() {
  const entries = [];
  const listeners = new Set();

  const emit = () => {
    for (const fn of listeners) fn();
  };

  return {
    entries,
    // Append an entry and notify the view. Defaults kind to "message" so the common case is
    // a one-field call; #358 passes an explicit kind for its change-card.
    push(entry) {
      const full = { kind: "message", ...entry };
      entries.push(full);
      emit();
      return full;
    },
    // Register a re-render callback; returns an unsubscribe. The view subscribes once at mount.
    subscribe(fn) {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
  };
}

// system-prompt.mjs — the model-facing system prompt for the web-chat agent.
//
// M1 SCOPE: this is a BARE PLACEHOLDER. The authoring behavior — the §1 lexicon rules, the §8
// skill-level register, the eight-contract usage guidance, the send-vs-swap policy — is the NEXT
// ticket (#356), which replaces this string. Keeping it a one-line placeholder here is deliberate:
// this ticket is plumbing only, and the proxy owns the system prompt (ADR-0054 §2) so #356 has a
// single, server-side seam to fill.

export const SYSTEM_PROMPT_PLACEHOLDER =
  "You are the reuben instrument-authoring assistant. (Placeholder — the authoring policy, " +
  "lexicon, and tool-usage guidance land in issue #356.)";

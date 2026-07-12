// Unit tests for the authoring-policy system prompt (issue #356) — a PROMPT LINT, not a model
// eval. This file cannot prove a live model obeys the prompt (there is no live LLM access on the
// merge-gating path); it proves the prompt TEXT actually states every rule #356's scope commits to,
// so a future edit can't silently drop a policy clause. Behavioral proof against a real model is
// `js/live-eval.mjs` (non-blocking, self-gated on ANTHROPIC_API_KEY); behavioral proof against the
// REAL loop/tool-layer/turn-envelope over a scripted mock model is `js/agent-policy-eval.test.mjs`.
//
// Run: `cd crates/reuben-web && node --test proxy/system-prompt.test.mjs`

import test from "node:test";
import assert from "node:assert";

import { SYSTEM_PROMPT, FORBIDDEN_TERMS, PLAIN_THEORY_PAIRS } from "./system-prompt.mjs";
import { RESTART_HONESTY_LINE } from "../js/agent-turn.mjs";

test("the prompt is non-empty and self-identifies the assistant", () => {
  assert.strictEqual(typeof SYSTEM_PROMPT, "string");
  assert.ok(SYSTEM_PROMPT.length > 200, "a bare placeholder is no longer acceptable (issue #356)");
  assert.match(SYSTEM_PROMPT, /reuben/i);
});

test("every forbidden term (spec §1) is explicitly named as forbidden in the prompt", () => {
  for (const term of FORBIDDEN_TERMS) {
    assert.ok(
      SYSTEM_PROMPT.toLowerCase().includes(term.toLowerCase()),
      `the prompt must instruct against "${term}"`,
    );
  }
});

test("the prompt states the never-say rule is about what reaches the USER, not the agent's own reasoning", () => {
  assert.match(SYSTEM_PROMPT, /never say/i);
  assert.match(SYSTEM_PROMPT, /reach(es)? the (person|user)/i);
});

test("the prompt carries every §1.2 plain/theory-aware pair", () => {
  for (const pair of PLAIN_THEORY_PAIRS) {
    assert.ok(SYSTEM_PROMPT.includes(pair.plain), `missing plain term for ${pair.dimension}`);
    assert.ok(SYSTEM_PROMPT.includes(pair.theory), `missing theory-aware term for ${pair.dimension}`);
  }
});

test("the prompt states the mirror rule (§1.2)", () => {
  assert.match(SYSTEM_PROMPT, /mirror/i);
});

test("the prompt states the register ratchet: default plain, bump on unprompted theory vocabulary, never demote, never ask (§8)", () => {
  assert.match(SYSTEM_PROMPT, /plain.*(default|start)/is);
  assert.match(SYSTEM_PROMPT, /unprompted/i);
  assert.match(SYSTEM_PROMPT, /never demote/i);
  assert.match(SYSTEM_PROMPT, /never ask/i);
  assert.match(SYSTEM_PROMPT, /echo.*doesn.t count|doesn.t count/is);
  assert.match(SYSTEM_PROMPT, /session/i);
});

test("the prompt states send-vs-swap routing without naming send/swap to the user (§6.1)", () => {
  // The routing guidance must exist...
  assert.match(SYSTEM_PROMPT, /restart/i);
  assert.match(SYSTEM_PROMPT, /no gap/i);
  // ...but the "how to talk about it" framing must say the split is invisible, never explained.
  assert.match(SYSTEM_PROMPT, /never (tell|explain).{0,80}(which|took|path)/is);
});

test("the prompt instructs validating a document before installing it (§5.1 case 3 habit)", () => {
  assert.match(SYSTEM_PROMPT, /valid.*before/is);
});

test("the prompt describes the streaming-plan-then-landing narration contract (§4.2/§4.5)", () => {
  assert.match(SYSTEM_PROMPT, /before.{0,40}(your )?tool/is);
  assert.match(SYSTEM_PROMPT, /sensory/i);
});

test("the prompt covers both turn-one shapes: describe-path echo/build/land and gallery-pick chips-verbatim (§2.3/§2.4)", () => {
  assert.match(SYSTEM_PROMPT, /verbatim/i);
  assert.match(SYSTEM_PROMPT, /next/i);
});

test("the prompt names the automatic, once-only first-restart framing without asking the model to author it per-turn (§6.4)", () => {
  assert.match(SYSTEM_PROMPT, /first.{0,20}restart|restart.{0,20}first/is);
  assert.match(SYSTEM_PROMPT, /automatic/i);
});

test("RESTART_HONESTY_LINE (agent-turn.mjs) is non-empty and jargon-free — safe at either register (§6.4 'inherits register')", () => {
  assert.strictEqual(typeof RESTART_HONESTY_LINE, "string");
  assert.ok(RESTART_HONESTY_LINE.length > 0);
  for (const term of FORBIDDEN_TERMS) {
    assert.ok(
      !RESTART_HONESTY_LINE.toLowerCase().includes(term.toLowerCase()),
      `the restart-honesty line must not contain the forbidden term "${term}"`,
    );
  }
});

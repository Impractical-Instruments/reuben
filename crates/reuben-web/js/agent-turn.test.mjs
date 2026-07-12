// Unit tests for the turn envelope (issue #354): the data contract #358 renders against. These
// pin the SHAPE and the §4.2 state transitions so the downstream UI ticket can bind to a stable
// contract.
//
// Run: `cd crates/reuben-web && node --test js/agent-turn.test.mjs`

import test from "node:test";
import assert from "node:assert";

import { userTurn, assistantTurn, stubTranscript } from "./agent-turn.mjs";

test("a user turn is plain, resolved, and carries the reserved slots (empty)", () => {
  const t = userTurn("make it brighter");
  assert.strictEqual(t.role, "user");
  assert.strictEqual(t.status, "resolved");
  assert.strictEqual(t.plan, "make it brighter");
  assert.strictEqual(t.diff, null);
  assert.deepStrictEqual(t.caveats, []);
  assert.deepStrictEqual(t.alternatives, []);
  assert.strictEqual(t.restartHonesty, null);
  assert.deepStrictEqual(t.toolLog, []);
});

test("an assistant turn starts in 'thinking' and streams its plan in place (§4.2)", () => {
  const b = assistantTurn();
  assert.strictEqual(b.turn.role, "assistant");
  assert.strictEqual(b.turn.status, "thinking");
  assert.strictEqual(b.turn.plan, "");

  b.appendPlan("Warming ");
  b.appendPlan("the top end");
  assert.strictEqual(b.turn.plan, "Warming the top end", "plan grows by appended tokens");

  const resolved = b.resolve();
  assert.strictEqual(resolved.status, "resolved", "thinking → resolved-in-place");
  assert.strictEqual(resolved, b.turn, "resolve returns the SAME object (mutated in place)");
});

test("setDiff attaches the structural node-identity diff (§4.6), survived always 0 on web", () => {
  const b = assistantTurn();
  b.setDiff({ survived: 0, added: ["/lfo"], removed: [], changed: ["/osc"] });
  assert.strictEqual(b.turn.diff.survived, 0);
  assert.deepStrictEqual(b.turn.diff.added, ["/lfo"]);
  assert.deepStrictEqual(b.turn.diff.changed, ["/osc"]);
});

test("recordTool logs tool provenance including a surfaced-to-agent error (not user-facing)", () => {
  const b = assistantTurn();
  b.recordTool({ id: "tu1", name: "send", input: { messages: [] }, result: { sent: 1 }, isError: false });
  b.recordTool({ id: "tu2", name: "swap", input: {}, result: "engine is not reachable", isError: true });
  assert.strictEqual(b.turn.toolLog.length, 2);
  assert.strictEqual(b.turn.toolLog[1].isError, true);
  // The error lives in toolLog (agent-facing provenance), NOT in the user-facing plan.
  assert.strictEqual(b.turn.plan, "");
});

test("the reserved slots exist but stay empty here (content is #356/M2's job)", () => {
  const t = assistantTurn().turn;
  assert.deepStrictEqual(t.caveats, []);
  assert.deepStrictEqual(t.alternatives, []);
  assert.strictEqual(t.restartHonesty, null);
});

test("turn ids are unique and stable per turn", () => {
  const a = userTurn("a").id;
  const b = assistantTurn().turn.id;
  const c = userTurn("c").id;
  assert.notStrictEqual(a, b);
  assert.notStrictEqual(b, c);
});

test("the stub transcript records ordered turns and every streamed delta", () => {
  const sink = stubTranscript();
  const u = userTurn("hi");
  sink.onUserTurn(u);
  const b = assistantTurn();
  sink.onTurnStart(b.turn);
  sink.onPlanDelta(b.turn, "a");
  sink.onPlanDelta(b.turn, "b");
  sink.onTurnResolved(b.resolve());
  assert.deepStrictEqual(sink.turns.map((t) => t.role), ["user", "assistant"]);
  assert.deepStrictEqual(sink.deltas.map((d) => d.text), ["a", "b"]);
});

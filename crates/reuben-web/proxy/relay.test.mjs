// Unit tests for the proxy relay core (issue #354, ADR-0054 §2/§5). Deterministic: a mock upstream
// fetch, no key, no network. Proves the self-gating (key-absent → clean 503, never crash), the
// model-facing request the proxy builds (system + tools + model + stream), and the SSE passthrough.
//
// Run: `cd crates/reuben-web && node --test proxy/relay.test.mjs`

import test from "node:test";
import assert from "node:assert";

import { createRelay, MODEL_DEFAULT } from "./relay.mjs";

const TOOLS = [{ name: "send", description: "dispatch", input_schema: { type: "object" } }];

test("no key → clean 503 with a telemetry code, never throws (ADR-0054 §5)", async () => {
  const relay = createRelay({ apiKey: undefined, systemPrompt: "sys", tools: TOOLS });
  const res = await relay({ messages: [{ role: "user", content: "hi" }] });
  assert.strictEqual(res.status, 503);
  const body = await res.json();
  assert.strictEqual(body.code, "proxy_unconfigured");
});

test("a body without messages → 400", async () => {
  const relay = createRelay({ apiKey: "sk-test", systemPrompt: "sys", tools: TOOLS });
  assert.strictEqual((await relay({})).status, 400);
  assert.strictEqual((await relay({ messages: [] })).status, 400);
});

test("with a key: declares system + tools + model + stream, and passes the SSE through", async () => {
  let captured;
  const fakeFetch = async (url, init) => {
    captured = { url, init };
    return new Response('event: message_stop\ndata: {"type":"message_stop"}\n\n', {
      status: 200,
      headers: { "content-type": "text/event-stream" },
    });
  };
  const relay = createRelay({
    apiKey: "sk-test",
    systemPrompt: "SYS-PLACEHOLDER",
    tools: TOOLS,
    fetchImpl: fakeFetch,
  });

  const res = await relay({ messages: [{ role: "user", content: "hi" }] });

  // Passthrough: status + SSE content-type + body reach the browser verbatim.
  assert.strictEqual(res.status, 200);
  assert.strictEqual(res.headers.get("content-type"), "text/event-stream");
  assert.match(await res.text(), /message_stop/);

  // The model-facing request the proxy owns (ADR-0054 §2).
  assert.strictEqual(captured.url, "https://api.anthropic.com/v1/messages");
  assert.strictEqual(captured.init.headers["x-api-key"], "sk-test");
  assert.strictEqual(captured.init.headers["anthropic-version"], "2023-06-01");
  const sent = JSON.parse(captured.init.body);
  assert.strictEqual(sent.model, MODEL_DEFAULT, "defaults to the Sonnet-5 tier (ADR-0054 §4)");
  assert.strictEqual(sent.system, "SYS-PLACEHOLDER");
  assert.strictEqual(sent.stream, true);
  assert.deepStrictEqual(sent.tools, TOOLS, "declares the generated tool schemas verbatim (§3)");
  assert.deepStrictEqual(sent.messages, [{ role: "user", content: "hi" }]);
  // M1 defaults thinking OFF: Sonnet-5's default-on adaptive thinking emits `thinking` blocks the
  // browser loop can't yet round-trip (400 `…thinking.thinking: Field required` on round 2).
  // Turning it on is #356's tuning call (ADR-0054 §4). A regression that drops this reds here
  // before it ever reaches the (non-blocking) live smoke.
  assert.deepStrictEqual(sent.thinking, { type: "disabled" });
});

test("an upstream network failure → 502, never throws (ADR-0054 §6)", async () => {
  const fakeFetch = async () => {
    throw new Error("ECONNRESET");
  };
  const relay = createRelay({ apiKey: "sk-test", systemPrompt: "s", tools: TOOLS, fetchImpl: fakeFetch });
  const res = await relay({ messages: [{ role: "user", content: "hi" }] });
  assert.strictEqual(res.status, 502);
  assert.strictEqual((await res.json()).code, "upstream_unreachable");
});

test("a non-2xx upstream (e.g. model overload) is forwarded with its status", async () => {
  const fakeFetch = async () =>
    new Response('{"type":"error","error":{"type":"overloaded_error"}}', {
      status: 529,
      headers: { "content-type": "application/json" },
    });
  const relay = createRelay({ apiKey: "sk-test", systemPrompt: "s", tools: TOOLS, fetchImpl: fakeFetch });
  const res = await relay({ messages: [{ role: "user", content: "hi" }] });
  // The browser transport checks response.ok before parsing SSE, so forwarding the status is enough.
  assert.strictEqual(res.status, 529);
});

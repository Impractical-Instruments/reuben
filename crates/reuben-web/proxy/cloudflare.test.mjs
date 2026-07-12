// Adapter-level tests for the mounted Cloudflare Pages Function entry (issue #403 / Live-loop/B,
// ADR-0054 §2/§3/§5). These sit one layer above relay.test.mjs: they drive `onRequestPost` /
// `onRequest` exactly as Pages does, proving the mount self-gates without a key and that the
// contract DECLARED to the model is the GENERATED artifact — not a hand-copy. Deterministic:
// no key for the gate cases; a stubbed `globalThis.fetch` for the one with-key case, no network.
//
// Run: `cd crates/reuben-web && node --test proxy/cloudflare.test.mjs`

import test from "node:test";
import assert from "node:assert";

import { onRequestPost, onRequest } from "./cloudflare.mjs";
import { SYSTEM_PROMPT } from "./system-prompt.mjs";
import { MODEL_DEFAULT } from "./relay.mjs";
import artifact from "../js/tool-schemas.generated.json" with { type: "json" };

/** A minimal Pages-style request: `onRequest*` only touches `.method` and `.json()`. */
function req(bodyObj, { method = "POST", badJson = false } = {}) {
  return {
    method,
    json: async () => {
      if (badJson) throw new SyntaxError("Unexpected token");
      return bodyObj;
    },
  };
}

test("no key → clean 503 proxy_unconfigured end-to-end through the mount (ADR-0054 §5)", async () => {
  const res = await onRequestPost({ request: req({ messages: [{ role: "user", content: "hi" }] }), env: {} });
  assert.strictEqual(res.status, 503);
  assert.strictEqual((await res.json()).code, "proxy_unconfigured");
});

test("a non-JSON body → 400 bad_request", async () => {
  const res = await onRequestPost({ request: req(null, { badJson: true }), env: { ANTHROPIC_API_KEY: "sk-test" } });
  assert.strictEqual(res.status, 400);
  assert.strictEqual((await res.json()).code, "bad_request");
});

test("onRequest rejects non-POST verbs with 405 (Pages catch-all)", async () => {
  const res = await onRequest({ request: req(null, { method: "GET" }), env: {} });
  assert.strictEqual(res.status, 405);
});

test("with a key: declares the GENERATED artifact + SYSTEM_PROMPT + default model (§2/§3/§4)", async () => {
  const savedFetch = globalThis.fetch;
  let captured;
  globalThis.fetch = async (url, init) => {
    captured = { url, init };
    return new Response('event: message_stop\ndata: {"type":"message_stop"}\n\n', {
      status: 200,
      headers: { "content-type": "text/event-stream" },
    });
  };
  try {
    const res = await onRequestPost({
      request: req({ messages: [{ role: "user", content: "make it warmer" }] }),
      env: { ANTHROPIC_API_KEY: "sk-test" },
    });

    // The SSE passthrough reaches the browser verbatim.
    assert.strictEqual(res.status, 200);
    assert.strictEqual(res.headers.get("content-type"), "text/event-stream");
    assert.match(await res.text(), /message_stop/);

    // Done-when #2: the tool schemas the proxy declares ARE the generated artifact (no drift).
    const sent = JSON.parse(captured.init.body);
    assert.ok(Array.isArray(artifact.tools) && artifact.tools.length > 0, "artifact carries tools");
    assert.deepStrictEqual(sent.tools, artifact.tools, "declares the generated tool schemas verbatim");
    assert.strictEqual(sent.system, SYSTEM_PROMPT, "declares the authoring-policy system prompt");
    assert.strictEqual(sent.model, MODEL_DEFAULT, "defaults to the Sonnet-5 tier (§4)");
    assert.strictEqual(captured.init.headers["x-api-key"], "sk-test");
  } finally {
    globalThis.fetch = savedFetch;
  }
});

test("REUBEN_CHAT_MODEL overrides the declared model id (config, not a constant — §4)", async () => {
  const savedFetch = globalThis.fetch;
  let captured;
  globalThis.fetch = async (url, init) => {
    captured = { init };
    return new Response('data: {"type":"message_stop"}\n\n', {
      status: 200,
      headers: { "content-type": "text/event-stream" },
    });
  };
  try {
    await onRequestPost({
      request: req({ messages: [{ role: "user", content: "hi" }] }),
      env: { ANTHROPIC_API_KEY: "sk-test", REUBEN_CHAT_MODEL: "claude-opus-4-8" },
    });
    assert.strictEqual(JSON.parse(captured.init.body).model, "claude-opus-4-8");
  } finally {
    globalThis.fetch = savedFetch;
  }
});

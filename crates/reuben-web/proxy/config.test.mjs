// Unit tests for the proxy env → config surface (issue #403 / Live-loop/B, ADR-0054 §4/§5).
// Deterministic, no key, no network.
//
// Run: `cd crates/reuben-web && node --test proxy/config.test.mjs`

import test from "node:test";
import assert from "node:assert";

import { readProxyConfig, RESHAPE_CEILING_DEFAULT } from "./config.mjs";
import { MODEL_DEFAULT } from "./relay.mjs";

test("empty env → self-gate posture: no key, default model + ceiling (ADR-0054 §4/§5)", () => {
  const c = readProxyConfig({});
  assert.strictEqual(c.apiKey, undefined, "no key ⇒ the relay self-gates 503");
  assert.strictEqual(c.model, MODEL_DEFAULT, "defaults to the Sonnet-5 tier (§4)");
  assert.strictEqual(c.reshapeCeiling, RESHAPE_CEILING_DEFAULT);
});

test("missing env argument is treated as empty (never throws)", () => {
  const c = readProxyConfig();
  assert.strictEqual(c.model, MODEL_DEFAULT);
  assert.strictEqual(c.reshapeCeiling, RESHAPE_CEILING_DEFAULT);
});

test("env overrides flow through: key, model id, and reshape ceiling N", () => {
  const c = readProxyConfig({
    ANTHROPIC_API_KEY: "sk-test",
    REUBEN_CHAT_MODEL: "claude-opus-4-8",
    REUBEN_CHAT_RESHAPE_CEILING: "12",
  });
  assert.strictEqual(c.apiKey, "sk-test");
  assert.strictEqual(c.model, "claude-opus-4-8");
  assert.strictEqual(c.reshapeCeiling, 12, "a valid override wins over the default");
});

test("a blank/invalid/non-positive ceiling override falls back to the default (never 500s)", () => {
  for (const raw of ["", "   ", "0", "-5", "3.5", "lots", "NaN"]) {
    assert.strictEqual(
      readProxyConfig({ REUBEN_CHAT_RESHAPE_CEILING: raw }).reshapeCeiling,
      RESHAPE_CEILING_DEFAULT,
      `override ${JSON.stringify(raw)} should fall back to the default`,
    );
  }
});

test("an empty REUBEN_CHAT_MODEL falls back to the Sonnet-5 tier default", () => {
  assert.strictEqual(readProxyConfig({ REUBEN_CHAT_MODEL: "" }).model, MODEL_DEFAULT);
});

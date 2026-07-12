// Unit tests for the generated tool-schema artifact (issue #354, ADR-0054 §3): the NO-DRIFT guard.
//
// The artifact `tool-schemas.generated.json` is produced by the Rust generator
// (src/tool_schema.rs, from reuben-core) and consumed by BOTH the proxy (declares to the model)
// and the in-page tool layer (executes). This test proves the EXECUTED contract (createToolLayer's
// tool names + the shapes the tool bodies read) matches the DECLARED contract in the artifact, so
// the two cannot drift. The Rust `committed_artifact_is_in_sync` test guards the other seam (the
// committed file vs the core generator).
//
// Run: `cd crates/reuben-web && node --test js/tool-schemas.test.mjs`

import test from "node:test";
import assert from "node:assert";
import { readFileSync } from "node:fs";

import { createToolLayer } from "./tools.mjs";

// Load the generated artifact without JSON import attributes (portable across node versions).
const artifact = JSON.parse(
  readFileSync(new URL("./tool-schemas.generated.json", import.meta.url), "utf8"),
);

// Minimal fakes: this test only inspects the tool ROSTER + input shapes, so the engine/introspect
// are never actually called — a bare object is enough to construct the layer.
const fakeEngine = { context: { state: "running" }, node: {}, currentBundle: () => null };
const fakeIntrospect = {
  describeOperators: () => [],
  describeInstrument: () => ({}),
  validate: () => ({ ok: true, errors: [], warnings: [] }),
  contentHash: () => "hash",
};

test("the artifact declares exactly the eight names the in-page layer executes (no drift)", () => {
  const declared = artifact.tools.map((t) => t.name).sort();
  const executed = Object.keys(createToolLayer({ engine: fakeEngine, introspect: fakeIntrospect })).sort();
  assert.deepStrictEqual(
    declared,
    executed,
    "declared (proxy) and executed (in-page) tool rosters must be identical",
  );
  assert.deepStrictEqual(declared, [
    "describe_instrument",
    "describe_operators",
    "engine_status",
    "get_current_instrument",
    "get_diagnostics",
    "send",
    "swap",
    "validate",
  ]);
});

test("every declared input_schema is an object schema (Anthropic tools API requirement)", () => {
  for (const t of artifact.tools) {
    assert.strictEqual(t.input_schema.type, "object", `${t.name} input_schema must be an object`);
    assert.strictEqual(typeof t.description, "string");
    assert.ok(t.description.length > 0, `${t.name} needs a description`);
  }
});

test("declared required fields match the shapes the tool bodies read (js/tools.mjs)", () => {
  const byName = Object.fromEntries(artifact.tools.map((t) => [t.name, t]));
  // send reads { messages }, swap/describe_instrument/validate read { document } (ADR-0052 §2 by-value).
  assert.deepStrictEqual(byName.send.input_schema.required, ["messages"]);
  assert.deepStrictEqual(byName.swap.input_schema.required, ["document"]);
  assert.deepStrictEqual(byName.describe_instrument.input_schema.required, ["document"]);
  assert.deepStrictEqual(byName.validate.input_schema.required, ["document"]);
  // The three no-input reads declare no required fields.
  for (const name of ["engine_status", "get_current_instrument", "get_diagnostics"]) {
    assert.ok(!byName[name].input_schema.required, `${name} takes no input`);
  }
  // swap's by-value inputs are declared (document + resources + expect), never a native `path`.
  const swapProps = byName.swap.input_schema.properties;
  assert.ok(swapProps.document && swapProps.resources && swapProps.expect);
  assert.ok(!("path" in swapProps), "web swap is by-value (ADR-0052 §2), never path-based");
});

test("the artifact carries the core-generated instrument schema + the Sonnet-5 default", () => {
  assert.strictEqual(artifact.instrument_schema.title, "reuben instrument");
  assert.strictEqual(artifact.model_default, "claude-sonnet-5");
});

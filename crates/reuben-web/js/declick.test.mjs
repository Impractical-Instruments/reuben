// declick.test.mjs — the raised-cosine declick ramp shape (spec §6.2.4, issue #360). Asserts the
// edge is a DECLICK, not a hard cut: it reaches its endpoints exactly, is monotonic, and — the
// load-bearing property — joins both ends with a flat tangent (the raised-cosine shape ADR-0050 §3
// fixes). A hard cut is a single step (max slope at the edge); a raised cosine has near-zero slope
// there. This is the automated half of §6.2.4's "no audible click"; the ear is the scripted-human
// half (restrike.spec.js header).

import { test } from "node:test";
import assert from "node:assert/strict";
import { raisedCosineRamp, raisedCosineDuckCurve, DECLICK_EDGE_MS, DECLICK_STEPS } from "./declick.mjs";

test("the down-edge fades 1 → 0, reaching silence EXACTLY (no residual tail)", () => {
  const down = raisedCosineRamp(1, 0);
  assert.equal(down[0], 1, "starts at unity");
  assert.equal(down[down.length - 1], 0, "reaches true silence, not an epsilon above it");
  // Symmetric about half-gain — the nearest-to-centre sample sits within a sample-step of 0.5
  // (the exact half-gain point falls between integer indices at even resolutions).
  assert.ok(Math.abs(down[Math.round((down.length - 1) / 2)] - 0.5) < 0.05, "half-gain near the midpoint");
});

test("the up-edge fades 0 → 1, reaching unity EXACTLY", () => {
  const up = raisedCosineRamp(0, 1);
  assert.equal(up[0], 0);
  assert.equal(up[up.length - 1], 1);
});

test("the ramp is monotonic — it never overshoots or ripples (a clean duck)", () => {
  const down = raisedCosineRamp(1, 0);
  for (let i = 1; i < down.length; i += 1) {
    assert.ok(down[i] <= down[i - 1] + 1e-7, `down-edge must not rise at ${i}`);
  }
  const up = raisedCosineRamp(0, 1);
  for (let i = 1; i < up.length; i += 1) {
    assert.ok(up[i] >= up[i - 1] - 1e-7, `up-edge must not fall at ${i}`);
  }
});

test("the edges join with a FLAT tangent — the raised-cosine property that removes the click", () => {
  // A hard cut is one step: slope ~1 across a single sample. A raised cosine leaves and arrives
  // with near-zero slope, so the first and last sample-to-sample deltas are far smaller than the
  // curve's steepest (mid-ramp) delta. Assert the join deltas are a small fraction of the max.
  const down = raisedCosineRamp(1, 0);
  const delta = (i) => Math.abs(down[i] - down[i - 1]);
  let maxDelta = 0;
  for (let i = 1; i < down.length; i += 1) maxDelta = Math.max(maxDelta, delta(i));
  const firstDelta = delta(1);
  const lastDelta = delta(down.length - 1);
  assert.ok(firstDelta < maxDelta * 0.1, "the fade LEAVES unity with a flat tangent (no step)");
  assert.ok(lastDelta < maxDelta * 0.1, "the fade ARRIVES at silence with a flat tangent (no step)");
});

test("the full duck is ONE curve, unity → true silence → unity, with the trough at the middle", () => {
  const duck = raisedCosineDuckCurve();
  assert.equal(duck[0], 1, "starts at unity");
  assert.equal(duck[duck.length - 1], 1, "returns to unity");
  const mid = (duck.length - 1) / 2;
  assert.equal(duck[mid], 0, "the trough is true silence, at the exact middle (where the caller commits)");
  // No doubled trough sample: exactly one zero (the shared midpoint), so it's a single clean curve.
  assert.equal(duck.filter((v) => v === 0).length, 1, "one silent sample, not a back-to-back seam");
  // 2·steps − 1 points: the two edges joined at their shared trough.
  assert.equal(duck.length, DECLICK_STEPS * 2 - 1);
});

test("fixed-and-observable per ADR-0050 §3: one edge duration, within the 5–20ms door", () => {
  assert.ok(DECLICK_EDGE_MS >= 5 && DECLICK_EDGE_MS <= 20, "edge stays inside ADR-0050 §3's tuning door");
  assert.ok(DECLICK_STEPS >= 2, "a ramp needs at least two points");
});

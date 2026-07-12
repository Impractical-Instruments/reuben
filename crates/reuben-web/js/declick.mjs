// declick.mjs — the raised-cosine declick ramp (spec §6.2.4, issue #360). A PURE curve helper,
// no Web Audio, no DOM — so it unit-tests under `node --test` while reuben-engine.mjs feeds it to
// a real AudioParam. It is the M1-web home of ADR-0050's declick PHILOSOPHY.
//
// WHERE THE DECLICK LIVES — and why it is here, not in the engine core (an #360 investigation
// finding). ADR-0050 §2 puts the declick as a MASTER-GAIN raised-cosine ramp on the core RT-side
// install slot, "inherited by both shells" — but §1 is explicit that ships with M2's mailbox swap:
// "M1 stays rude — no M1 work item," and §Context records that no master-gain stage exists on the
// core master path at all yet. M1's swap is the STOP-THE-WORLD restart-swap (ADR-0046 §10): the
// web player tears the worklet's engine down and reconstructs it (destroy → stage → construct,
// ADR-0052 §2, tools.mjs `swap` → engine.loadBundle). So in M1-web there is NO engine-side gain to
// ramp — the only master-gain the restart edges can duck is a Web Audio GainNode in the SHELL's
// graph (reuben-engine.mjs interposes one between the worklet node and the destination). This
// module is that duck's SHAPE; reuben-engine.mjs `restrikeDuck` is its application. We reproduce
// ADR-0050 §3's fixed-and-observable contract exactly: raised-cosine, nominal ~10ms per edge,
// tunable only within 5–20ms, no knob. When M2's core ramp lands, this becomes redundant and the
// web shell inherits the core one — recorded so the duplication is a conscious interim, not drift.

// ADR-0050 §3: one duration, one shape, hard-coded — raised-cosine, nominal 10ms per edge, tunable
// within 5–20ms without a new decision. We pick 12ms: long enough to declick any transient at
// 44.1/48kHz, short enough to read as instant (§6.2.3 — a beat, not a wait).
export const DECLICK_EDGE_MS = 12;

// Curve resolution: points in one edge's value-curve. 64 samples over ~12ms is far finer than the
// ear's click threshold and keeps setValueCurveAtTime cheap; the SHAPE (raised cosine), not the
// point count, is what removes the click.
export const DECLICK_STEPS = 64;

/**
 * A raised-cosine (Hann-family) S-curve ramp from `from` to `to` over `steps` points — the declick
 * edge shape (spec §6.2.4 / ADR-0050 §3). The weight `w = 0.5 - 0.5·cos(π·i/(steps-1))` eases
 * 0→1 with zero slope at BOTH ends, so the value leaves `from` and arrives at `to` with no
 * derivative discontinuity — that flat-tangent join is precisely what a hard cut (a step, infinite
 * slope) lacks and what makes the edge inaudible. Endpoints are pinned EXACTLY to `from`/`to` (not
 * left to float on rounding) so a fade-to-silence truly reaches 0 — a residual epsilon on the last
 * sample is a quiet tail, not silence.
 *
 * @param {number} from  gain the edge starts at (1 for the down-edge, 0 for the up-edge).
 * @param {number} to    gain the edge ends at (0 for the down-edge, 1 for the up-edge).
 * @param {number} [steps] curve resolution (default DECLICK_STEPS).
 * @returns {Float32Array} the ramp, ready for AudioParam.setValueCurveAtTime.
 */
export function raisedCosineRamp(from, to, steps = DECLICK_STEPS) {
  const n = Math.max(2, steps | 0);
  const curve = new Float32Array(n);
  for (let i = 0; i < n; i += 1) {
    const w = 0.5 - 0.5 * Math.cos((Math.PI * i) / (n - 1));
    curve[i] = from + (to - from) * w;
  }
  // Pin the endpoints exactly — the cos() may leave a rounding epsilon that would keep the trough
  // just above silence (an audible tail) or the peak just below unity.
  curve[0] = from;
  curve[n - 1] = to;
  return curve;
}

/**
 * The full duck as ONE value curve: fade unity → silence → unity, each edge a raised cosine (spec
 * §6.2.4). Returned as a SINGLE curve (not two back-to-back) on purpose: Web Audio's
 * setValueCurveAtTime rejects a second curve whose start touches the first's end
 * ("overlaps another curve") — scheduling the whole duck as one array sidesteps that boundary and
 * keeps the trough a shared, exactly-zero sample. The down-edge is the first `steps` points and the
 * up-edge the next `steps`, joined at the silent midpoint (the duplicate 0 is dropped), so the
 * curve has `2·steps − 1` points and its middle sample is true silence — where the caller commits.
 *
 * @param {number} [steps] resolution PER edge (default DECLICK_STEPS).
 * @returns {Float32Array} the 1 → 0 → 1 duck, ready for AudioParam.setValueCurveAtTime.
 */
export function raisedCosineDuckCurve(steps = DECLICK_STEPS) {
  const down = raisedCosineRamp(1, 0, steps);
  const up = raisedCosineRamp(0, 1, steps);
  const duck = new Float32Array(down.length + up.length - 1);
  duck.set(down, 0);
  duck.set(up.subarray(1), down.length); // skip up[0] (== 0) so the trough isn't a doubled sample
  return duck;
}

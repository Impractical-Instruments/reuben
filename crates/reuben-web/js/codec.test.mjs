// Unit tests for the control-buffer codec — specifically decodeControl, added for the
// share-link sidecar (the snapshot entries are verbatim encodeControl() buffers the player
// reads back to seed widget state).
//
// Pure JS, `node --test`, NO browser and NO wasm, following the share.test.mjs precedent.
// encodeControl itself is pinned byte-for-byte by the Rust side (codec.rs `exact_wire_layout`)
// and exercised end-to-end by check.mjs; here the proof is that decodeControl is its inverse,
// plus the malformed-buffer rejections.
//
// Run: `cd crates/reuben-web && node --test js/codec.test.mjs`

import test from "node:test";
import assert from "node:assert";

import { encodeControl, decodeControl } from "./codec.mjs";

test("round-trip — an f32 arg (the fader/param-toggle shape)", () => {
  const out = decodeControl(encodeControl("/kick_step7/in", [1]));
  assert.strictEqual(out.address, "/kick_step7/in");
  assert.deepStrictEqual(out.args, [1]);
});

test("round-trip — f32 survives as the nearest f32 (wire is 4-byte float)", () => {
  const out = decodeControl(encodeControl("/tempo/in", [132.5]));
  assert.deepStrictEqual(out.args, [132.5]); // exactly representable in f32
  const lossy = decodeControl(encodeControl("/tempo/in", [0.1]));
  assert.strictEqual(lossy.args[0], Math.fround(0.1));
});

test("round-trip — an {i32} arg (the chord-degree marker)", () => {
  const out = decodeControl(encodeControl("/chord/in", [{ i32: -5 }, 1]));
  assert.deepStrictEqual(out.args, [{ i32: -5 }, 1]);
});

test("round-trip — a string arg", () => {
  const out = decodeControl(encodeControl("/mode/in", ["dorian"]));
  assert.deepStrictEqual(out.args, ["dorian"]);
});

test("round-trip — zero args", () => {
  const out = decodeControl(encodeControl("/ping/in"));
  assert.deepStrictEqual(out.args, []);
});

test("a truncated buffer throws, never mis-reads", () => {
  const buf = encodeControl("/tempo/in", [120]);
  for (const cut of [3, buf.length - 1]) {
    assert.throws(() => decodeControl(buf.subarray(0, cut)), RangeError);
  }
});

test("an unknown arg tag throws", () => {
  const buf = encodeControl("/tempo/in", [120]);
  // The tag byte sits right after u32 addr_len + addr + u32 arg count.
  const tagPos = 4 + "/tempo/in".length + 4;
  const evil = buf.slice();
  evil[tagPos] = 7;
  assert.throws(() => decodeControl(evil), TypeError);
});

test("a subarray view decodes (byteOffset respected)", () => {
  const buf = encodeControl("/tone/in", [0.5]);
  const padded = new Uint8Array(buf.length + 8);
  padded.set(buf, 8);
  const out = decodeControl(padded.subarray(8));
  assert.strictEqual(out.address, "/tone/in");
});

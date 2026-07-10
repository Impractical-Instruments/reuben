// Unit tests for the share-link envelope codec (issue #228, P6).
//
// Pure JS, `node --test`, NO browser and NO wasm — share.mjs is Node-importable by design
// (the README generator and the CI decode checker both use it). Following the
// surface/*.test.mjs precedent.
//
// Run: `cd crates/reuben-web && node --test js/share.test.mjs`
//
// The proofs come in two kinds:
//   1. Round-trip — encode(bundle) → decode → byte-identical bundle, over doc-only,
//      doc+resources, and doc+snapshot bundles. The document must survive byte-identically
//      (AC 2); resource bytes and snapshot buffers survive verbatim.
//   2. Adversarial — every failure class the decoder must reject: an envelope from the
//      future (r2.), a truncated/garbage payload, a deflate bomb over the size cap, a
//      hand-crafted TLV with an oversized length prefix, a kind=1 (sample) resource, and a
//      resource-count overflow. Each asserts the ShareError `code`, because main.js maps
//      that code — not the message text — onto the A–I banner copy.

import test from "node:test";
import assert from "node:assert";

import {
  encodeBundle,
  decodeBundle,
  ShareError,
  ENVELOPE_PREFIX,
  CAPS,
} from "./share.mjs";

const enc = new TextEncoder();

// A tiny, real-shaped instrument document (the byte content is opaque to the codec — it
// carries doc text verbatim and never parses it).
const DOC = JSON.stringify({ format_version: 2, instrument: "vibrato", nodes: [] });

function bytesEqual(a, b) {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}

// --- round-trip --------------------------------------------------------------------------

test("doc-only bundle round-trips byte-identically", async () => {
  const fragment = await encodeBundle({ docText: DOC });
  assert.ok(fragment.startsWith(ENVELOPE_PREFIX), "fragment carries the r1. prefix");
  const out = await decodeBundle(fragment);
  assert.strictEqual(out.docText, DOC);
  assert.deepStrictEqual(out.resources, []);
  assert.deepStrictEqual(out.snapshot, []);
  assert.strictEqual(out.surfaceText, null);
});

test("decode tolerates a leading # (location.hash form)", async () => {
  const fragment = await encodeBundle({ docText: DOC });
  const out = await decodeBundle(`#${fragment}`);
  assert.strictEqual(out.docText, DOC);
});

test("bundle with text resources round-trips (keys, kinds, bytes verbatim)", async () => {
  const resources = [
    { key: "voices/kick-voice.json", kind: 0, bytes: enc.encode('{"instrument":"kick"}') },
    { key: "voices/snare-voice.json", kind: 0, bytes: enc.encode('{"instrument":"snare"}') },
  ];
  const out = await decodeBundle(await encodeBundle({ docText: DOC, resources }));
  assert.strictEqual(out.resources.length, 2);
  for (let i = 0; i < resources.length; i++) {
    assert.strictEqual(out.resources[i].key, resources[i].key);
    assert.strictEqual(out.resources[i].kind, resources[i].kind);
    assert.ok(bytesEqual(out.resources[i].bytes, resources[i].bytes), "resource bytes verbatim");
  }
});

test("encode accepts a Map of resources (the engine's discovery shape)", async () => {
  const map = new Map([["a.json", { kind: 0, bytes: enc.encode("{}") }]]);
  const out = await decodeBundle(await encodeBundle({ docText: DOC, resources: map }));
  assert.strictEqual(out.resources[0].key, "a.json");
});

test("snapshot buffers round-trip verbatim (the control-state sidecar)", async () => {
  // Opaque to the codec — it carries encodeControl() output byte-for-byte.
  const snapshot = [new Uint8Array([1, 2, 3, 4]), new Uint8Array([9, 8, 7])];
  const out = await decodeBundle(await encodeBundle({ docText: DOC, snapshot }));
  assert.strictEqual(out.snapshot.length, 2);
  assert.ok(bytesEqual(out.snapshot[0], snapshot[0]));
  assert.ok(bytesEqual(out.snapshot[1], snapshot[1]));
});

// --- extension sections: the surface doc travels as a tagged trailing section --------------

const SURFACE = JSON.stringify({
  surface_version: 1,
  instrument: "vibrato",
  controls: [{ bind: "depth", widget: "fader" }],
});

test("surfaceText round-trips verbatim alongside resources and a snapshot", async () => {
  const resources = [{ key: "voices/a.json", kind: 0, bytes: enc.encode("{}") }];
  const snapshot = [new Uint8Array([1, 2, 3])];
  const out = await decodeBundle(
    await encodeBundle({ docText: DOC, resources, snapshot, surfaceText: SURFACE }),
  );
  assert.strictEqual(out.surfaceText, SURFACE);
  assert.strictEqual(out.docText, DOC);
  assert.strictEqual(out.resources.length, 1);
  assert.strictEqual(out.snapshot.length, 1);
});

test("a surface-less bundle stays byte-identical to a day-one bundle (no empty section)", async () => {
  const withNull = await encodeBundle({ docText: DOC, surfaceText: null });
  const without = await encodeBundle({ docText: DOC });
  assert.strictEqual(withNull, without);
});

// Craft a full valid DAY-ONE-layout TLV (doc + 0 resources + 0 snapshots, nothing after) —
// the exact bytes an old player minted. The new decoder must read it with surfaceText null:
// this is the back-compat pin for every link shared before surfaces travelled.
function dayOneTlv(docBytes) {
  const tlv = new Uint8Array(4 + docBytes.length + 4 + 4);
  const v = new DataView(tlv.buffer);
  let p = 0;
  v.setUint32(p, docBytes.length, true); p += 4;
  tlv.set(docBytes, p); p += docBytes.length;
  v.setUint32(p, 0, true); p += 4; // resource count
  v.setUint32(p, 0, true); // snapshot count
  return tlv;
}

test("back-compat — a day-one-layout bundle (no trailing sections) decodes with surfaceText null", async () => {
  const out = await decodeBundle(await forge(dayOneTlv(enc.encode(DOC))));
  assert.strictEqual(out.docText, DOC);
  assert.strictEqual(out.surfaceText, null);
});

// Append one extension section (u8 tag, u32 len, payload) to a TLV.
function withSection(tlv, tag, payload) {
  const out = new Uint8Array(tlv.length + 1 + 4 + payload.length);
  out.set(tlv, 0);
  let p = tlv.length;
  out[p++] = tag;
  new DataView(out.buffer).setUint32(p, payload.length, true);
  p += 4;
  out.set(payload, p);
  return out;
}

test("forward-compat — an unknown trailing tag is skipped; a surface after it still reads", async () => {
  let tlv = dayOneTlv(enc.encode(DOC));
  tlv = withSection(tlv, 99, enc.encode("mystery bytes from the future"));
  tlv = withSection(tlv, 1, enc.encode(SURFACE));
  const out = await decodeBundle(await forge(tlv));
  assert.strictEqual(out.surfaceText, SURFACE);
});

test("duplicate surface sections — the last one wins", async () => {
  let tlv = dayOneTlv(enc.encode(DOC));
  tlv = withSection(tlv, 1, enc.encode("{}"));
  tlv = withSection(tlv, 1, enc.encode(SURFACE));
  const out = await decodeBundle(await forge(tlv));
  assert.strictEqual(out.surfaceText, SURFACE);
});

test("class E — a truncated extension section (declared longer than present) is damaged", async () => {
  let tlv = dayOneTlv(enc.encode(DOC));
  tlv = withSection(tlv, 1, enc.encode(SURFACE));
  await expectCode(decodeBundle(await forge(tlv.subarray(0, tlv.length - 3))), "damaged");
});

test("class E — an extension section over the per-section cap is damaged, not allocated", async () => {
  // Declare a section bigger than the cap with no body — the cap check fires before readBytes.
  const tlv = dayOneTlv(enc.encode(DOC));
  const out = new Uint8Array(tlv.length + 1 + 4);
  out.set(tlv, 0);
  out[tlv.length] = 1;
  new DataView(out.buffer).setUint32(tlv.length + 1, CAPS.PER_RESOURCE_BYTES + 1, true);
  await expectCode(decodeBundle(await forge(out)), "damaged");
});

// --- adversarial: a fragment from a text message is untrusted input -----------------------

// Forge a fragment from ARBITRARY TLV bytes so we can craft payloads the encoder would never
// mint — this is exactly the untrusted shape a hostile link carries. Deflate + base64url only;
// no prefix logic, so we can also swap the prefix.
async function forge(tlvBytes, prefix = ENVELOPE_PREFIX) {
  const cs = new CompressionStream("deflate-raw");
  const w = cs.writable.getWriter();
  w.write(tlvBytes);
  w.close();
  const comp = new Uint8Array(await new Response(cs.readable).arrayBuffer());
  return prefix + Buffer.from(comp).toString("base64url");
}

async function expectCode(promise, code) {
  await assert.rejects(promise, (e) => {
    assert.ok(e instanceof ShareError, `expected ShareError, got ${e}`);
    assert.strictEqual(e.code, code, `expected code "${code}", got "${e.code}"`);
    return true;
  });
}

test("class B — an envelope from the future (r2.) is refused, not decompressed", async () => {
  await expectCode(decodeBundle("r2.anything"), "future");
});

test("class A/C — a non-r1 hash (#about) is a plain damaged link", async () => {
  await expectCode(decodeBundle("#about"), "damaged");
});

test("class C — invalid base64url is damaged", async () => {
  await expectCode(decodeBundle("r1.not base64!!"), "damaged");
});

test("class C — a truncated/garbage deflate payload is damaged", async () => {
  await expectCode(decodeBundle("r1.AAAA"), "damaged");
});

test("class E — an oversized length prefix throws before allocating", async () => {
  // docLen = 0xFFFFFFFF with no doc body: readBytes must refuse, not attempt a 4 GB slice.
  const tlv = new Uint8Array(4);
  new DataView(tlv.buffer).setUint32(0, 0xffffffff, true);
  await expectCode(decodeBundle(await forge(tlv)), "damaged");
});

test("class E — a truncated TLV (declared doc longer than present) is damaged", async () => {
  const tlv = new Uint8Array(4 + 4); // docLen=10 but only 4 body bytes follow
  const v = new DataView(tlv.buffer);
  v.setUint32(0, 10, true);
  await expectCode(decodeBundle(await forge(tlv)), "damaged");
});

test("class E′ — a kind=1 (sample) resource in the TLV is rejected before its data is read", async () => {
  // doc="{}", 1 resource, key="a", kind=1, then (never reached) a data length.
  const key = enc.encode("a");
  const doc = enc.encode("{}");
  const tlv = new Uint8Array(4 + doc.length + 4 + 4 + key.length + 1);
  const v = new DataView(tlv.buffer);
  let p = 0;
  v.setUint32(p, doc.length, true); p += 4;
  tlv.set(doc, p); p += doc.length;
  v.setUint32(p, 1, true); p += 4; // resource count
  v.setUint32(p, key.length, true); p += 4;
  tlv.set(key, p); p += key.length;
  tlv[p] = 1; // kind = 1 (sample)
  await expectCode(decodeBundle(await forge(tlv)), "sample");
});

test("class E — a resource-count overflow fails at parse, before any resource is read", async () => {
  const doc = enc.encode("{}");
  const tlv = new Uint8Array(4 + doc.length + 4);
  const v = new DataView(tlv.buffer);
  v.setUint32(0, doc.length, true);
  tlv.set(doc, 4);
  v.setUint32(4 + doc.length, CAPS.RESOURCE_COUNT + 1, true); // 65 resources declared
  await expectCode(decodeBundle(await forge(tlv)), "damaged");
});

test("class D — a deflate bomb over the decompressed cap is aborted streaming", async () => {
  // 2 MB of one byte compresses to a few KB (well under the 16 KB fragment cap) but inflates
  // past the 1 MB decompressed cap — the streaming abort must fire.
  const huge = "z".repeat(2 * 1024 * 1024);
  const fragment = await encodeBundle({ docText: huge });
  assert.ok(fragment.length <= CAPS.FRAGMENT_BYTES, "the compressed fragment stays small");
  await expectCode(decodeBundle(fragment), "too-large");
});

test("mint refuses a kind=1 resource (AC 5 — a sample document cannot be minted)", async () => {
  const resources = [{ key: "samples/blip.wav", kind: 1, bytes: new Uint8Array(44) }];
  await expectCode(encodeBundle({ docText: DOC, resources }), "sample");
});

// Share-link envelope codec for the reuben web player (issue #228, P6).
//
// The link IS the unit of an instrument: a whole playable bundle travels in the URL
// fragment, so "share" == "send a text message" (borrowed from Strudel). This module is the
// one owner of that wire format — it encodes a bundle to a `#r1.…` fragment and decodes one
// back, with every attacker-controlled length bounds-checked and every size capped.
//
// ENVIRONMENT-AGNOSTIC (ADR-0041): pure ES module, no bundler, no DOM. It runs on the main
// thread (main.js: boot from location.hash, mint on Share), in Node for the README link
// generator (web/scripts/gen-share-links.mjs), and in Node for the CI decode-and-compare
// checker. Its only platform dependency is CompressionStream/DecompressionStream with the
// 'deflate-raw' format, a global in modern browsers and Node ≥18.
//
// WHAT THE LINK CARRIES — a BUNDLE, not a document (ADR-0042, decision 1). An instrument
// document is not self-contained: groovebox references three voice patches via its
// `resources` table, which the browser normally resolves by FETCHING them from the origin. A
// bare-document link would boot only where those files happen to be served. So the envelope
// carries the document PLUS every resource the engine's discovery pass collected — the decoder
// hands them back and main.js feeds a bundle-backed fetchResource that never touches the
// network. Plus a control-state SIDECAR (ADR-0042, decision 2): the untouched document round-
// trips byte-identically, and a snapshot of the controls the player moved travels alongside it.
//
// TEXT RESOURCES ONLY. `kind = 1` (WAV) is refused at mint AND at parse — a TRUST BOUNDARY,
// not a size limit (ADR-0042, decision 3): sample bytes are the only bytes in a link that
// reach a hand-rolled binary parser (`hound` via decode.rs), which trusts the WAV header's
// declared length. Excluding kind=1 means zero hostile bytes reach it.
//
// Envelope layout:
//
//   r1.<base64url(  deflate-raw(  TLV  )  )>
//   ^^^                                        literal version prefix, OUTSIDE the compression
//
// The prefix is readable WITHOUT decompressing (ADR-0042, decision 4), so "a link from the
// future", "a truncated link", and "someone pasted #about" are distinct failures, not one
// useless decompression error. `deflate-raw` over gzip: no mtime/OS bytes, ~18 bytes smaller.
//
// TLV payload (little-endian, byte-aligned, no padding — codec.mjs's idiom):
//
//   u32            doc byte length
//   [u8]           doc UTF-8 bytes
//   u32            resource count
//     per resource: u32 key_len, [u8] key, u8 kind (0=text; 1 REJECTED),
//                   u32 data_len, [u8] data
//   u32            snapshot count
//     per entry:   u32 buf_len, [u8]   — a verbatim encodeControl() buffer
//
// Every u32 length above is attacker-controlled. readU32/readBytes validate against the
// remaining buffer before allocating, the caps below bound the totals, and decompression is
// capped STREAMING (aborted past the limit, never checked after the fact) so a 4 KB fragment
// cannot inflate to gigabytes.

/** The envelope version prefix. `r1.` is the layout THIS module reads and writes; a higher
 *  number (e.g. `r2.`) is a link from a newer reuben and fails with code "future". Distinct
 *  from the document's `format_version` — see ADR-0042, decision 4. */
export const ENVELOPE_PREFIX = "r1.";

/** Hard caps. Guardrails, not budgets — 4× the largest real document, far past any real rig
 *  (groovebox needs 3 resources). Enforced at mint and at boot. */
export const CAPS = {
  /** Whole fragment (the `r1.…` string), mint and boot. A sanity ceiling. */
  FRAGMENT_BYTES: 16 * 1024,
  /** Decompressed TLV payload — the deflate-bomb guard, enforced streaming. */
  DECOMPRESSED_BYTES: 1024 * 1024,
  /** Resources in one bundle. */
  RESOURCE_COUNT: 64,
  /** Bytes in one resource — bounded by the 1 MB total, but named so a failure blames a key. */
  PER_RESOURCE_BYTES: 256 * 1024,
};

/** A resource kind that this envelope refuses to carry (WAV samples). */
const KIND_SAMPLE = 1;

/**
 * A decode/encode failure carrying a `code` that main.js maps onto the A–I banner copy —
 * callers switch on the code, never the (developer-facing) message. Codes:
 *   - "future"     — envelope version newer than this module (`r2.` …). Failure class B.
 *   - "damaged"    — bad base64/deflate, truncated payload, or malformed TLV. Classes C, E.
 *   - "too-large"  — fragment over cap, or decompressed payload over cap. Class D.
 *   - "sample"     — the TLV declares a `kind = 1` (WAV) resource. Class E′.
 */
export class ShareError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "ShareError";
    this.code = code;
  }
}

const encoder = new TextEncoder();
const decoder = new TextDecoder();

// --- base64url (no padding, URL-safe alphabet) -------------------------------------------
//
// Hand-rolled rather than Buffer/btoa so the module has one behaviour in every environment
// (browser main thread, Node generator, Node CI) and the tests pin exact bytes.

const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const B64_INV = /* @__PURE__ */ (() => {
  const t = new Int16Array(128).fill(-1);
  for (let i = 0; i < B64.length; i++) t[B64.charCodeAt(i)] = i;
  return t;
})();

function toBase64Url(bytes) {
  let out = "";
  for (let i = 0; i < bytes.length; i += 3) {
    const b0 = bytes[i];
    const b1 = i + 1 < bytes.length ? bytes[i + 1] : 0;
    const b2 = i + 2 < bytes.length ? bytes[i + 2] : 0;
    out += B64[b0 >> 2];
    out += B64[((b0 & 0x03) << 4) | (b1 >> 4)];
    if (i + 1 < bytes.length) out += B64[((b1 & 0x0f) << 2) | (b2 >> 6)];
    if (i + 2 < bytes.length) out += B64[b2 & 0x3f];
  }
  return out;
}

function fromBase64Url(str) {
  let acc = 0;
  let bits = 0;
  // Ceil-safe output length: every 4 input chars carry 3 bytes; a trailing 2 or 3 chars carry
  // 1 or 2. (n*6 >> 3) is exactly that count.
  const out = new Uint8Array((str.length * 6) >> 3);
  let oi = 0;
  for (let i = 0; i < str.length; i++) {
    const c = str.charCodeAt(i);
    const v = c < 128 ? B64_INV[c] : -1;
    if (v < 0) throw new ShareError("damaged", `invalid base64url character at ${i}`);
    acc = (acc << 6) | v;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out[oi++] = (acc >> bits) & 0xff;
    }
  }
  return oi === out.length ? out : out.subarray(0, oi);
}

// --- deflate-raw (streaming both ways) ---------------------------------------------------

async function collectStream(readable, cap) {
  const reader = readable.getReader();
  const chunks = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.length;
    // Cap enforced HERE, streaming — abort before accumulating past the limit, so a bomb
    // can't balloon memory before a post-hoc check would catch it.
    if (cap != null && total > cap) {
      await reader.cancel();
      throw new ShareError("too-large", `decompressed payload exceeds ${cap} bytes`);
    }
    chunks.push(value);
  }
  const out = new Uint8Array(total);
  let o = 0;
  for (const c of chunks) {
    out.set(c, o);
    o += c.length;
  }
  return out;
}

async function deflateRaw(bytes) {
  const cs = new CompressionStream("deflate-raw");
  const writer = cs.writable.getWriter();
  // Pump input without awaiting before we read: for large payloads writer backpressure only
  // clears as the readable is drained, so awaiting write()/close() up front could deadlock.
  // Errors surface on the readable side, so the writer promise is intentionally detached.
  const pump = (async () => {
    await writer.write(bytes);
    await writer.close();
  })();
  pump.catch(() => {});
  const out = await collectStream(cs.readable, null);
  await pump;
  return out;
}

async function inflateRawCapped(bytes, cap) {
  const ds = new DecompressionStream("deflate-raw");
  const writer = ds.writable.getWriter();
  const pump = (async () => {
    await writer.write(bytes);
    await writer.close();
  })();
  pump.catch(() => {});
  try {
    return await collectStream(ds.readable, cap);
  } catch (e) {
    if (e instanceof ShareError) throw e; // the streaming size-cap abort
    throw new ShareError("damaged", `deflate-raw decode failed: ${e.message}`);
  }
}

// --- bounds-checked TLV reader -----------------------------------------------------------

function readU32(view, off) {
  if (off + 4 > view.byteLength) {
    throw new ShareError("damaged", "truncated: u32 length runs past the buffer");
  }
  return view.getUint32(off, true);
}

function readU8(view, off) {
  if (off + 1 > view.byteLength) {
    throw new ShareError("damaged", "truncated: byte runs past the buffer");
  }
  return view.getUint8(off);
}

// Slice `len` bytes at `off`, but only after proving they are present — the whole point of the
// exercise (a data_len of 0xFFFFFFFF must throw, not allocate 4 GB).
function readBytes(bytes, off, len) {
  if (off + len > bytes.length) {
    throw new ShareError("damaged", `truncated: ${len}-byte field runs past the buffer`);
  }
  return bytes.subarray(off, off + len);
}

// --- encode ------------------------------------------------------------------------------

// Accept resources as a Map (key -> {kind, bytes}, the engine's discovery shape) or as an
// array of {key, kind, bytes}. Normalise to the array form.
function normalizeResources(resources) {
  if (resources == null) return [];
  if (resources instanceof Map) {
    return [...resources].map(([key, { kind, bytes }]) => ({ key, kind, bytes }));
  }
  return resources;
}

/**
 * Encode a bundle into a `r1.…` fragment (no leading `#`; callers prepend it for a URL).
 *
 * @param {object} bundle
 * @param {string} bundle.docText - the top-level instrument document, carried verbatim.
 * @param {Map<string,{kind:number,bytes:Uint8Array}>|Array<{key:string,kind:number,bytes:Uint8Array}>} [bundle.resources]
 * @param {Uint8Array[]} [bundle.snapshot] - verbatim encodeControl() buffers (the sidecar).
 * @returns {Promise<string>} the `r1.…` fragment.
 * @throws {ShareError} code "sample" if any resource is kind=1; "too-large" if the fragment
 *   exceeds the 16 KB cap.
 */
export async function encodeBundle({ docText, resources, snapshot } = {}) {
  if (typeof docText !== "string") {
    throw new TypeError("encodeBundle: docText must be a string");
  }
  const res = normalizeResources(resources);
  const snaps = snapshot ?? [];

  const docBytes = encoder.encode(docText);
  const resParts = res.map((r) => {
    if (r.kind === KIND_SAMPLE) {
      // Refused at MINT (AC 5) — a kind=1 document cannot be turned into a link at all. The
      // reason is the trust boundary (ADR-0042, decision 3), not the size.
      throw new ShareError("sample", `resource ${r.key} is a sample (kind=1) — not shareable`);
    }
    const keyBytes = encoder.encode(r.key);
    const data = r.bytes instanceof Uint8Array ? r.bytes : new Uint8Array(r.bytes);
    return { keyBytes, kind: r.kind, data };
  });

  // Size pass (codec.mjs's two-pass idiom): exact byte count before allocation.
  let size = 4 + docBytes.length + 4;
  for (const p of resParts) size += 4 + p.keyBytes.length + 1 + 4 + p.data.length;
  size += 4;
  for (const s of snaps) size += 4 + s.length;

  const out = new Uint8Array(size);
  const view = new DataView(out.buffer);
  let pos = 0;

  view.setUint32(pos, docBytes.length, true);
  pos += 4;
  out.set(docBytes, pos);
  pos += docBytes.length;

  view.setUint32(pos, resParts.length, true);
  pos += 4;
  for (const p of resParts) {
    view.setUint32(pos, p.keyBytes.length, true);
    pos += 4;
    out.set(p.keyBytes, pos);
    pos += p.keyBytes.length;
    out[pos++] = p.kind;
    view.setUint32(pos, p.data.length, true);
    pos += 4;
    out.set(p.data, pos);
    pos += p.data.length;
  }

  view.setUint32(pos, snaps.length, true);
  pos += 4;
  for (const s of snaps) {
    view.setUint32(pos, s.length, true);
    pos += 4;
    out.set(s, pos);
    pos += s.length;
  }

  const compressed = await deflateRaw(out);
  const fragment = ENVELOPE_PREFIX + toBase64Url(compressed);
  if (fragment.length > CAPS.FRAGMENT_BYTES) {
    throw new ShareError(
      "too-large",
      `fragment is ${fragment.length} bytes, over the ${CAPS.FRAGMENT_BYTES}-byte cap`,
    );
  }
  return fragment;
}

// --- decode ------------------------------------------------------------------------------

/**
 * Decode a `r1.…` fragment (a leading `#`, as in location.hash, is tolerated) back into a
 * bundle. Does NOT parse the document — it returns docText verbatim, so a JSON or version
 * failure is the caller's (the engine's message, failure classes F/G/H). This layer owns only
 * the envelope: version, base64, deflate, and the bounds-checked TLV.
 *
 * @param {string} fragment
 * @returns {Promise<{docText:string, resources:Array<{key:string,kind:number,bytes:Uint8Array}>, snapshot:Uint8Array[]}>}
 * @throws {ShareError} with a `code` for each failure class (see ShareError).
 */
export async function decodeBundle(fragment) {
  if (typeof fragment !== "string") {
    throw new ShareError("damaged", "fragment must be a string");
  }
  const s = fragment.startsWith("#") ? fragment.slice(1) : fragment;

  // Version FIRST, without decompressing (ADR-0042, decision 4).
  if (!s.startsWith(ENVELOPE_PREFIX)) {
    if (/^r\d+\./.test(s)) {
      throw new ShareError("future", "envelope was made by a newer version of reuben");
    }
    throw new ShareError("damaged", "not a reuben share link");
  }
  // Fragment length cap at boot, too — the whole `r1.…` string.
  if (s.length > CAPS.FRAGMENT_BYTES) {
    throw new ShareError("too-large", `fragment exceeds the ${CAPS.FRAGMENT_BYTES}-byte cap`);
  }

  const compressed = fromBase64Url(s.slice(ENVELOPE_PREFIX.length));
  const payload = await inflateRawCapped(compressed, CAPS.DECOMPRESSED_BYTES);
  const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);

  let pos = 0;

  const docLen = readU32(view, pos);
  pos += 4;
  const docBytes = readBytes(payload, pos, docLen);
  pos += docLen;
  const docText = decoder.decode(docBytes);

  const resCount = readU32(view, pos);
  pos += 4;
  // Cap the count BEFORE the read loop (ADR §"bounds checking"): reject a malformed envelope at
  // parse rather than after mutating any shell state.
  if (resCount > CAPS.RESOURCE_COUNT) {
    throw new ShareError("damaged", `resource count ${resCount} exceeds the cap of ${CAPS.RESOURCE_COUNT}`);
  }
  const resources = [];
  for (let i = 0; i < resCount; i++) {
    const keyLen = readU32(view, pos);
    pos += 4;
    const keyBytes = readBytes(payload, pos, keyLen);
    pos += keyLen;
    const key = decoder.decode(keyBytes);
    const kind = readU8(view, pos);
    pos += 1;
    if (kind === KIND_SAMPLE) {
      // Refused at parse, BEFORE its data is read or any stage_resource runs (AC 5). Class E′.
      throw new ShareError("sample", `resource ${key} is a sample (kind=1) — not shareable`);
    }
    const dataLen = readU32(view, pos);
    pos += 4;
    if (dataLen > CAPS.PER_RESOURCE_BYTES) {
      throw new ShareError("damaged", `resource ${key} is ${dataLen} bytes, over the per-resource cap`);
    }
    const data = readBytes(payload, pos, dataLen);
    pos += dataLen;
    resources.push({ key, kind, bytes: data.slice() });
  }

  const snapCount = readU32(view, pos);
  pos += 4;
  if (snapCount > CAPS.RESOURCE_COUNT) {
    throw new ShareError("damaged", `snapshot count ${snapCount} exceeds the cap`);
  }
  const snapshot = [];
  for (let i = 0; i < snapCount; i++) {
    const bufLen = readU32(view, pos);
    pos += 4;
    const buf = readBytes(payload, pos, bufLen);
    pos += bufLen;
    snapshot.push(buf.slice());
  }

  return { docText, resources, snapshot };
}

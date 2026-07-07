// Control-buffer encoder for the reuben web player (issue #224).
//
// Codes against `crates/reuben-web/src/codec.rs` — the control channel v1 flat tagged
// wire format. That file's `exact_wire_layout` test is the byte-for-byte spec of what
// `encodeControl` must produce; the Rust `decode_control` is the consumer.
//
// Wire format (little-endian throughout, byte-aligned, no padding):
//
//   u32              address byte length
//   [u8; addr_len]   UTF-8 address bytes (e.g. "/clock/tempo")
//   u32              arg count
//   per arg:
//     u8             tag (TAG_F32 | TAG_I32 | TAG_STR)
//     payload:
//       TAG_F32:     4 bytes, LE f32
//       TAG_I32:     4 bytes, LE i32
//       TAG_STR:     u32 LE byte length + UTF-8 bytes
//
// Arg mapping (ADR-0030's flat-primitive form):
//   bare JS number  -> F32   (numeric control args are floats unless explicitly marked)
//   { i32: n }      -> I32   (the explicit integer marker)
//   string          -> Str
// Anything else throws — the channel carries exactly these three primitives.

/** Wire tag for an F32 payload (4 bytes, LE f32). */
export const TAG_F32 = 0;
/** Wire tag for an I32 payload (4 bytes, LE i32). */
export const TAG_I32 = 1;
/** Wire tag for a Str payload (u32 LE byte length + UTF-8 bytes). */
export const TAG_STR = 2;

const encoder = new TextEncoder();

/**
 * Encode one control message into a control-channel v1 buffer.
 *
 * @param {string} address - the control address, e.g. "/clock/tempo"
 * @param {Array<number | string | {i32: number}>} args - flat primitive args
 * @returns {Uint8Array} the encoded buffer (its .buffer is transfer-safe)
 */
export function encodeControl(address, args = []) {
  if (typeof address !== "string") {
    throw new TypeError(`encodeControl: address must be a string, got ${typeof address}`);
  }
  if (!Array.isArray(args)) {
    throw new TypeError("encodeControl: args must be an array");
  }

  // Pass 1: normalize each arg to { tag, payload-shape } and pre-encode strings so the
  // exact buffer size is known before allocation.
  const addressBytes = encoder.encode(address);
  let size = 4 + addressBytes.length + 4; // addr len + addr + arg count
  const normalized = args.map((arg, i) => {
    if (typeof arg === "number") {
      size += 1 + 4;
      return { tag: TAG_F32, num: arg };
    }
    if (typeof arg === "string") {
      const bytes = encoder.encode(arg);
      size += 1 + 4 + bytes.length;
      return { tag: TAG_STR, bytes };
    }
    if (
      arg !== null &&
      typeof arg === "object" &&
      "i32" in arg &&
      typeof arg.i32 === "number" &&
      Number.isInteger(arg.i32)
    ) {
      size += 1 + 4;
      return { tag: TAG_I32, num: arg.i32 };
    }
    throw new TypeError(
      `encodeControl: arg ${i} must be a number (F32), {i32: n} (I32), or string (Str), ` +
        `got ${arg === null ? "null" : typeof arg}`,
    );
  });

  // Pass 2: fill the buffer.
  const out = new Uint8Array(size);
  const view = new DataView(out.buffer);
  let pos = 0;

  view.setUint32(pos, addressBytes.length, true);
  pos += 4;
  out.set(addressBytes, pos);
  pos += addressBytes.length;
  view.setUint32(pos, normalized.length, true);
  pos += 4;

  for (const arg of normalized) {
    out[pos++] = arg.tag;
    switch (arg.tag) {
      case TAG_F32:
        view.setFloat32(pos, arg.num, true);
        pos += 4;
        break;
      case TAG_I32:
        view.setInt32(pos, arg.num, true);
        pos += 4;
        break;
      case TAG_STR:
        view.setUint32(pos, arg.bytes.length, true);
        pos += 4;
        out.set(arg.bytes, pos);
        pos += arg.bytes.length;
        break;
    }
  }

  return out;
}

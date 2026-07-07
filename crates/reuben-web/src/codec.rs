//! Control channel v1 — the flat tagged codec between the shell's JS and the worklet's WASM
//! engine (issue #224, ADR-0030's flat-primitive form).
//!
//! The main thread sends `{address, args}` over `port.postMessage`; the worklet's JS packs a
//! flat tagged byte buffer into WASM memory and calls `queue_control(ptr, len)`; this module
//! decodes those bytes back into `(String, Vec<Arg>)` for `Engine::queue_osc(&address, &args)`.
//!
//! Why a hand-rolled codec instead of an existing wire format:
//!
//! - **Not `rosc`**: the control channel is not OSC-the-binary-protocol. It carries ADR-0030's
//!   *flat primitive form* — an address plus a list of `F32`/`I32`/`Str` atoms — and nothing
//!   else. Pulling in an OSC packet library would buy type tags, bundles, timetags, and
//!   4-byte padding we would immediately have to forbid, for a channel whose whole vocabulary
//!   is three primitives.
//! - **Not `js-sys` structured values**: the worklet boundary is a raw `(ptr, len)` into WASM
//!   linear memory, precisely so the Rust side stays a plain byte decoder — fully portable,
//!   host-testable with `cargo test`, no wasm-bindgen types anywhere near the engine.
//!
//! The JS encoder mirrors [`encode_control`] byte for byte (the `exact_wire_layout` test below
//! is its spec). One JS-side convention worth stating here because it shapes the tag set:
//! **bare JS numbers default to `F32`** — a control message's numeric args are floats unless
//! the sender explicitly marks an integer, matching how `Arg::as_f32` treats params.
//!
//! ## Wire format
//!
//! Little-endian throughout, byte-aligned, no padding:
//!
//! ```text
//! u32              address byte length
//! [u8; addr_len]   UTF-8 address bytes (e.g. "/clock/tempo")
//! u32              arg count
//! per arg:
//!   u8             tag (TAG_F32 | TAG_I32 | TAG_STR)
//!   payload:
//!     TAG_F32:     4 bytes, LE f32
//!     TAG_I32:     4 bytes, LE i32
//!     TAG_STR:     u32 LE byte length + UTF-8 bytes
//! ```
//!
//! Trailing bytes after the last arg are an error — that strictness exists to catch drift in
//! the JS encoder (a miscounted length field would otherwise decode "successfully" and corrupt
//! the *next* message's framing silently).
//!
//! Decoding is defensive: every length field is bounds-checked with overflow-safe arithmetic
//! before slicing, so a hostile or garbage buffer can never panic — it returns a
//! [`CodecError`], which the shell logs and drops.

use reuben_core::message::Arg;
use std::sync::Arc;

/// Wire tag for an [`Arg::F32`] payload (4 bytes, LE f32).
pub const TAG_F32: u8 = 0;
/// Wire tag for an [`Arg::I32`] payload (4 bytes, LE i32).
pub const TAG_I32: u8 = 1;
/// Wire tag for an [`Arg::Str`] payload (u32 LE byte length + UTF-8 bytes).
pub const TAG_STR: u8 = 2;

/// Why a control buffer failed to decode.
///
/// Errors are diagnostics for the shell's log channel, not control flow: a bad buffer means
/// the JS encoder and this decoder have drifted (or memory got corrupted), so the message is
/// logged and dropped — there is no retry or fallback path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    /// Ran out of bytes mid-field (header, tag, or payload).
    Truncated,
    /// An arg tag byte that is none of [`TAG_F32`] / [`TAG_I32`] / [`TAG_STR`].
    BadTag(u8),
    /// The address or a `Str` payload was not valid UTF-8.
    BadUtf8,
    /// Bytes remained after the last declared arg — the count of unconsumed bytes.
    TrailingBytes(usize),
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodecError::Truncated => write!(f, "control buffer truncated mid-field"),
            CodecError::BadTag(t) => write!(f, "unknown control arg tag {t}"),
            CodecError::BadUtf8 => write!(f, "control address or string payload is not UTF-8"),
            CodecError::TrailingBytes(n) => {
                write!(f, "{n} trailing byte(s) after last control arg")
            }
        }
    }
}

impl std::error::Error for CodecError {}

/// A bounds-checked read head over the incoming buffer. Every advance goes through
/// [`Cursor::take`], whose checked arithmetic is the single place an attacker-controlled
/// length meets a slice — no other code indexes `bytes`.
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    /// Consume exactly `n` bytes, or [`CodecError::Truncated`] if fewer remain. The
    /// `checked_add` matters on 32-bit targets (wasm32): `pos + n` with two
    /// attacker-influenced values near `u32::MAX` would otherwise wrap and slice garbage.
    fn take(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        let end = self.pos.checked_add(n).ok_or(CodecError::Truncated)?;
        let slice = self.bytes.get(self.pos..end).ok_or(CodecError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    fn read_u8(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }

    fn read_u32(&mut self) -> Result<u32, CodecError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_f32(&mut self) -> Result<f32, CodecError> {
        let b = self.take(4)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_i32(&mut self) -> Result<i32, CodecError> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// A length-prefixed UTF-8 string field (the address, or a `Str` payload).
    fn read_str(&mut self) -> Result<&'a str, CodecError> {
        let len = self.read_u32()? as usize;
        let bytes = self.take(len)?;
        std::str::from_utf8(bytes).map_err(|_| CodecError::BadUtf8)
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }
}

/// Decode one control-channel v1 buffer into the address and flat primitive args for
/// `Engine::queue_osc`.
///
/// Total: never panics on any input. Length fields are bounds-checked before slicing, and the
/// declared arg count is trusted only one arg at a time — a hostile `count` with too few bytes
/// behind it fails [`CodecError::Truncated`] on its first missing tag, before any
/// proportional allocation happens.
pub fn decode_control(bytes: &[u8]) -> Result<(String, Vec<Arg>), CodecError> {
    let mut cur = Cursor { bytes, pos: 0 };

    let address = cur.read_str()?.to_owned();
    let count = cur.read_u32()? as usize;

    // Each arg costs at least 1 byte (its tag), so the remaining byte count bounds any honest
    // `count`. Capping the pre-allocation there keeps a hostile `count: u32::MAX` from
    // reserving gigabytes before the first Truncated error.
    let mut args = Vec::with_capacity(count.min(cur.remaining()));
    for _ in 0..count {
        let tag = cur.read_u8()?;
        match tag {
            TAG_F32 => args.push(Arg::F32(cur.read_f32()?)),
            TAG_I32 => args.push(Arg::I32(cur.read_i32()?)),
            TAG_STR => args.push(Arg::Str(Arc::from(cur.read_str()?))),
            other => return Err(CodecError::BadTag(other)),
        }
    }

    match cur.remaining() {
        0 => Ok((address, args)),
        n => Err(CodecError::TrailingBytes(n)),
    }
}

/// Encode an address and flat primitive args into a control-channel v1 buffer — the exact
/// inverse of [`decode_control`].
///
/// The engine never sends on this channel; this function exists as the executable reference
/// for the JS-side encoder (PR C writes against the `exact_wire_layout` test) and to power the
/// round-trip tests that pin the format.
///
/// Non-primitive [`Arg`]s (`Note`, `Harmony`, `Enum`, `F32Buffer`) are a caller bug: the
/// control channel carries only ADR-0030's flat primitive form *by construction* — vocab
/// structs cross the boundary already flattened to primitives (`OscArg::to_osc`), and buffers
/// never cross at all. So they `debug_assert!` (loud in tests and dev builds, where the bug is
/// fixable) and are skipped in release (a panic in the audio shell would be worse than a
/// dropped arg; a silent skip in *all* builds would hide the bug, hence the assert). The
/// emitted count matches the args actually written, so the output always decodes cleanly.
pub fn encode_control(address: &str, args: &[Arg]) -> Vec<u8> {
    fn is_primitive(arg: &Arg) -> bool {
        matches!(arg, Arg::F32(_) | Arg::I32(_) | Arg::Str(_))
    }

    let mut out = Vec::new();
    out.extend_from_slice(&(address.len() as u32).to_le_bytes());
    out.extend_from_slice(address.as_bytes());

    let count = args.iter().filter(|a| is_primitive(a)).count() as u32;
    out.extend_from_slice(&count.to_le_bytes());

    for arg in args {
        match arg {
            Arg::F32(v) => {
                out.push(TAG_F32);
                out.extend_from_slice(&v.to_le_bytes());
            }
            Arg::I32(v) => {
                out.push(TAG_I32);
                out.extend_from_slice(&v.to_le_bytes());
            }
            Arg::Str(s) => {
                out.push(TAG_STR);
                out.extend_from_slice(&(s.len() as u32).to_le_bytes());
                out.extend_from_slice(s.as_bytes());
            }
            other => {
                debug_assert!(
                    false,
                    "control channel carries only flat primitive Args (ADR-0030), got {other:?}"
                );
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(address: &str, args: &[Arg]) {
        let bytes = encode_control(address, args);
        let (addr, decoded) = decode_control(&bytes).expect("round-trip decode");
        assert_eq!(addr, address);
        assert_eq!(decoded, args);
    }

    /// The wire layout, byte by byte, over all three tags. **This test is the spec for the
    /// JS-side encoder (PR C)**: JS must produce exactly these bytes for this message.
    #[test]
    fn exact_wire_layout() {
        let bytes = encode_control("/a", &[Arg::F32(1.5), Arg::I32(-2), Arg::Str("hi".into())]);
        #[rustfmt::skip]
        let expected: Vec<u8> = vec![
            2, 0, 0, 0,             // address byte length = 2 (u32 LE)
            b'/', b'a',             // "/a"
            3, 0, 0, 0,             // arg count = 3 (u32 LE)
            TAG_F32,                // tag 0
            0x00, 0x00, 0xC0, 0x3F, // 1.5f32 LE
            TAG_I32,                // tag 1
            0xFE, 0xFF, 0xFF, 0xFF, // -2i32 LE
            TAG_STR,                // tag 2
            2, 0, 0, 0,             // string byte length = 2 (u32 LE)
            b'h', b'i',             // "hi"
        ];
        assert_eq!(bytes, expected);
        let (addr, args) = decode_control(&expected).unwrap();
        assert_eq!(addr, "/a");
        assert_eq!(
            args,
            vec![Arg::F32(1.5), Arg::I32(-2), Arg::Str("hi".into())]
        );
    }

    #[test]
    fn round_trips_primitive_mixes() {
        round_trip("/clock/tempo", &[Arg::F32(120.0)]);
        round_trip("/voicer/note", &[Arg::I32(60), Arg::F32(0.8)]);
        round_trip(
            "/sampler/load",
            &[
                Arg::Str("kick.wav".into()),
                Arg::I32(3),
                Arg::Str("".into()), // empty string payload is legal
                Arg::F32(-0.25),
            ],
        );
    }

    #[test]
    fn round_trips_empty_args() {
        round_trip("/transport/start", &[]);
    }

    #[test]
    fn round_trips_empty_address() {
        // Nonsensical as OSC, but the codec is a dumb pipe: framing must not care.
        round_trip("", &[Arg::I32(1)]);
        round_trip("", &[]);
    }

    #[test]
    fn round_trips_non_ascii() {
        round_trip("/läge/温度", &[Arg::Str("héllo, 世界".into())]);
    }

    /// Truncation at *every* byte boundary of a valid buffer: never panics, and — because a
    /// valid buffer has no trailing bytes and fixed-position length fields — always fails
    /// `Truncated` specifically.
    #[test]
    fn truncation_at_every_boundary_never_panics() {
        let full = encode_control(
            "/clock/tempo",
            &[Arg::F32(120.0), Arg::I32(7), Arg::Str("swing".into())],
        );
        for len in 0..full.len() {
            assert_eq!(
                decode_control(&full[..len]),
                Err(CodecError::Truncated),
                "prefix of {len} bytes"
            );
        }
        assert!(decode_control(&full).is_ok());
    }

    #[test]
    fn bad_tag_is_reported() {
        let mut bytes = encode_control("/x", &[]);
        // Rewrite the count to 1 and append an unknown tag.
        let count_at = bytes.len() - 4;
        bytes[count_at..].copy_from_slice(&1u32.to_le_bytes());
        bytes.push(9);
        assert_eq!(decode_control(&bytes), Err(CodecError::BadTag(9)));
    }

    #[test]
    fn bad_utf8_in_address() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&[0xFF, 0xFE]); // invalid UTF-8
        bytes.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(decode_control(&bytes), Err(CodecError::BadUtf8));
    }

    #[test]
    fn bad_utf8_in_str_payload() {
        let mut bytes = encode_control("/x", &[]);
        let count_at = bytes.len() - 4;
        bytes[count_at..].copy_from_slice(&1u32.to_le_bytes());
        bytes.push(TAG_STR);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.push(0x80); // lone continuation byte
        assert_eq!(decode_control(&bytes), Err(CodecError::BadUtf8));
    }

    #[test]
    fn trailing_bytes_are_an_error() {
        let mut bytes = encode_control("/x", &[Arg::F32(1.0)]);
        bytes.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        assert_eq!(decode_control(&bytes), Err(CodecError::TrailingBytes(3)));
    }

    /// Hostile length fields near `u32::MAX` must bounds-check (and, on 32-bit targets, not
    /// overflow `pos + len`) rather than panic or slice out of range.
    #[test]
    fn hostile_lengths_do_not_panic() {
        // Address length far beyond the buffer.
        let mut bytes = u32::MAX.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"tiny");
        assert_eq!(decode_control(&bytes), Err(CodecError::Truncated));

        // Str payload length far beyond the buffer.
        let mut bytes = encode_control("/x", &[]);
        let count_at = bytes.len() - 4;
        bytes[count_at..].copy_from_slice(&1u32.to_le_bytes());
        bytes.push(TAG_STR);
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        bytes.push(0);
        assert_eq!(decode_control(&bytes), Err(CodecError::Truncated));

        // Hostile arg count with no bytes behind it: fails on the first missing tag.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u32.to_le_bytes()); // empty address
        bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // absurd count
        assert_eq!(decode_control(&bytes), Err(CodecError::Truncated));
    }

    /// A non-primitive Arg is a caller bug: loud in debug builds (this test), skipped in
    /// release so the count stays consistent (see `encode_control` docs).
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "flat primitive")]
    fn non_primitive_arg_asserts_in_debug() {
        encode_control("/x", &[Arg::Enum(3)]);
    }
}

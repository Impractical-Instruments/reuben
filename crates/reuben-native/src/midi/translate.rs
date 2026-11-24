//! Decoding one MIDI message into the translated form the router matches against: its kind,
//! channel and controller, and the `/midi/<ch>/…` address a log line and `send_midi` spell it as.
//!
//! | MIDI message | Address | Args |
//! | --- | --- | --- |
//! | note on, velocity > 0 | `/midi/<ch>/note` | `[pitch f32, velocity/127 f32]` |
//! | note off, or note on velocity 0 | `/midi/<ch>/note` | `[pitch f32, 0.0]` |
//! | control change | `/midi/<ch>/cc/<n>` | `[value/127 f32]` |
//! | pitch bend | `/midi/<ch>/bend` | `[-1..1 f32]`, 8192 → 0.0 |
//! | channel pressure | `/midi/<ch>/pressure` | `[value/127 f32]` |
//! | program change | `/midi/<ch>/program` | `[program i32]` |
//!
//! Channels are 1–16, as printed on hardware.

use reuben_api::render::Arg;
use reuben_api::surface::MidiKind;

/// One MIDI message in translated form.
#[derive(Debug, Clone, PartialEq)]
pub struct Translated {
    pub kind: MidiKind,
    /// 1..=16.
    pub channel: u8,
    /// A control change's controller number; `None` for every other kind.
    pub controller: Option<u8>,
    pub address: String,
    pub args: Vec<Arg>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decoded {
    Message(Translated),
    /// A channel message of a kind the map cannot bind (polyphonic aftertouch), or bytes that are
    /// not one well-formed message.
    Unsupported,
    /// System and realtime messages (clock, active sensing, SysEx), which keyboards send
    /// constantly: neither routed nor counted.
    Ignored,
}

/// Decodes one complete MIDI message, as `midir` delivers it (running status already expanded).
pub fn translate(bytes: &[u8]) -> Decoded {
    let Some((&status, data)) = bytes.split_first() else {
        return Decoded::Ignored;
    };
    if status >= 0xF0 {
        return Decoded::Ignored;
    }
    if status < 0x80 || data.iter().any(|&b| b >= 0x80) {
        return Decoded::Unsupported;
    }
    let channel = (status & 0x0F) + 1;
    let kind = match status >> 4 {
        0x8 | 0x9 => MidiKind::Note,
        0xB => MidiKind::Cc,
        0xC => MidiKind::Program,
        0xD => MidiKind::Pressure,
        0xE => MidiKind::Bend,
        _ => return Decoded::Unsupported,
    };
    let expected = match kind {
        MidiKind::Program | MidiKind::Pressure => 1,
        _ => 2,
    };
    if data.len() != expected {
        return Decoded::Unsupported;
    }
    let unit = |v: u8| f32::from(v) / 127.0;
    let (address, args) = match kind {
        MidiKind::Note => {
            let velocity = if status >> 4 == 0x8 {
                0.0
            } else {
                unit(data[1])
            };
            (
                format!("/midi/{channel}/note"),
                vec![Arg::F32(f32::from(data[0])), Arg::F32(velocity)],
            )
        }
        MidiKind::Cc => (
            format!("/midi/{channel}/cc/{}", data[0]),
            vec![Arg::F32(unit(data[1]))],
        ),
        MidiKind::Bend => {
            let raw = i32::from(data[0]) | (i32::from(data[1]) << 7);
            (format!("/midi/{channel}/bend"), vec![Arg::F32(bend(raw))])
        }
        MidiKind::Pressure => (
            format!("/midi/{channel}/pressure"),
            vec![Arg::F32(unit(data[0]))],
        ),
        MidiKind::Program => (
            format!("/midi/{channel}/program"),
            vec![Arg::I32(i32::from(data[0]))],
        ),
    };
    Decoded::Message(Translated {
        kind,
        channel,
        controller: (kind == MidiKind::Cc).then_some(data[0]),
        address,
        args,
    })
}

/// 14-bit bend to -1..1 with the centre, 8192, exactly 0: the two halves have different widths
/// (8192 steps below, 8191 above), so each is divided by its own.
fn bend(raw: i32) -> f32 {
    let offset = raw - 8192;
    if offset < 0 {
        offset as f32 / 8192.0
    } else {
        offset as f32 / 8191.0
    }
}

/// Reads an address and args as the translator would have produced them, for injecting a message
/// without hardware. `None` for anything no device could send: a channel outside 1–16 or spelled
/// with a leading zero, a controller or pitch outside 0–127, a value outside its range, the wrong
/// number of args. Integer args are taken where floats are expected.
pub fn parse_translated(address: &str, args: &[Arg]) -> Option<Translated> {
    let segments: Vec<&str> = address.strip_prefix("/midi/")?.split('/').collect();
    let channel = data_number(segments.first()?)?;
    if !(1..=16).contains(&channel) {
        return None;
    }
    let channel = channel as u8;
    let kind = MidiKind::ALL
        .into_iter()
        .find(|k| Some(&k.name()) == segments.get(1))?;
    let segment_count = if kind == MidiKind::Cc { 3 } else { 2 };
    if segments.len() != segment_count {
        return None;
    }
    let controller = match kind {
        MidiKind::Cc => Some(
            u8::try_from(data_number(segments[2])?)
                .ok()
                .filter(|n| *n <= 127)?,
        ),
        _ => None,
    };
    let args = match (kind, args) {
        (MidiKind::Note, [pitch, velocity]) => {
            let pitch = number(pitch)?;
            if pitch.fract() != 0.0 || !(0.0..=127.0).contains(&pitch) {
                return None;
            }
            vec![Arg::F32(pitch), Arg::F32(within(velocity, 0.0)?)]
        }
        (MidiKind::Cc | MidiKind::Pressure, [value]) => vec![Arg::F32(within(value, 0.0)?)],
        (MidiKind::Bend, [value]) => vec![Arg::F32(within(value, -1.0)?)],
        (MidiKind::Program, [Arg::I32(program)]) if (0..=127).contains(program) => {
            vec![Arg::I32(*program)]
        }
        _ => return None,
    };
    Some(Translated {
        kind,
        channel,
        controller,
        address: address.to_string(),
        args,
    })
}

/// A decimal in canonical spelling (no sign, no leading zero), so each translated message has one
/// address, which a log line prints.
fn data_number(text: &str) -> Option<u32> {
    let canonical = !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    canonical.then(|| text.parse().ok()).flatten()
}

fn number(arg: &Arg) -> Option<f32> {
    match arg {
        Arg::F32(v) if v.is_finite() => Some(*v),
        Arg::I32(v) => Some(*v as f32),
        _ => None,
    }
}

/// `arg` as a float in `low..=1.0`.
fn within(arg: &Arg, low: f32) -> Option<f32> {
    number(arg).filter(|v| (low..=1.0).contains(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(bytes: &[u8]) -> Translated {
        match translate(bytes) {
            Decoded::Message(t) => t,
            other => panic!("{bytes:02X?} decoded to {other:?}"),
        }
    }

    /// Each supported message's bytes and its translation: address, args, kind, channel.
    type Case = (Vec<u8>, &'static str, Vec<Arg>, MidiKind, u8);

    fn cases() -> Vec<Case> {
        vec![
            (
                vec![0x90, 60, 100],
                "/midi/1/note",
                vec![Arg::F32(60.0), Arg::F32(100.0 / 127.0)],
                MidiKind::Note,
                1,
            ),
            (
                vec![0x80, 60, 64],
                "/midi/1/note",
                vec![Arg::F32(60.0), Arg::F32(0.0)],
                MidiKind::Note,
                1,
            ),
            (
                vec![0x90, 60, 0],
                "/midi/1/note",
                vec![Arg::F32(60.0), Arg::F32(0.0)],
                MidiKind::Note,
                1,
            ),
            (
                vec![0xB4, 74, 127],
                "/midi/5/cc/74",
                vec![Arg::F32(1.0)],
                MidiKind::Cc,
                5,
            ),
            (
                vec![0xE0, 0x00, 0x40],
                "/midi/1/bend",
                vec![Arg::F32(0.0)],
                MidiKind::Bend,
                1,
            ),
            (
                vec![0xE0, 0, 0],
                "/midi/1/bend",
                vec![Arg::F32(-1.0)],
                MidiKind::Bend,
                1,
            ),
            (
                vec![0xE0, 0x7F, 0x7F],
                "/midi/1/bend",
                vec![Arg::F32(1.0)],
                MidiKind::Bend,
                1,
            ),
            (
                vec![0xD1, 64],
                "/midi/2/pressure",
                vec![Arg::F32(64.0 / 127.0)],
                MidiKind::Pressure,
                2,
            ),
            (
                vec![0xCF, 5],
                "/midi/16/program",
                vec![Arg::I32(5)],
                MidiKind::Program,
                16,
            ),
        ]
    }

    #[test]
    fn translates_each_supported_kind() {
        for (bytes, address, args, kind, channel) in cases() {
            let t = message(&bytes);
            assert_eq!(t.address, address, "{bytes:02X?}");
            assert_eq!(t.args, args, "{bytes:02X?}");
            assert_eq!(t.kind, kind, "{bytes:02X?}");
            assert_eq!(t.channel, channel, "{bytes:02X?}");
            let controller = (kind == MidiKind::Cc).then_some(bytes[1]);
            assert_eq!(t.controller, controller, "{bytes:02X?}");
        }
    }

    #[test]
    fn translate_ignores_realtime_and_system() {
        for bytes in [&[0xF8][..], &[0xFE], &[0xF0, 1, 2, 0xF7], &[]] {
            assert!(matches!(translate(bytes), Decoded::Ignored), "{bytes:02X?}");
        }
    }

    #[test]
    fn poly_aftertouch_is_unsupported() {
        assert!(matches!(translate(&[0xA0, 60, 10]), Decoded::Unsupported));
    }

    #[test]
    fn malformed_messages_are_unsupported() {
        for bytes in [
            &[0x90, 60][..],
            &[0x90, 60, 100, 1],
            &[0x3C, 100],
            &[0xB0, 0x80, 1],
        ] {
            assert!(
                matches!(translate(bytes), Decoded::Unsupported),
                "{bytes:02X?}"
            );
        }
    }

    #[test]
    fn parse_translated_round_trips() {
        for (bytes, ..) in cases() {
            let t = message(&bytes);
            assert_eq!(parse_translated(&t.address, &t.args), Some(t));
        }
    }

    #[test]
    fn parse_translated_takes_integers_for_floats() {
        assert_eq!(
            parse_translated("/midi/1/note", &[Arg::I32(60), Arg::I32(1)]),
            Some(message(&[0x90, 60, 127]))
        );
    }

    #[test]
    fn parse_translated_refuses_what_no_device_sends() {
        for (address, args) in [
            ("/midi/17/note", vec![Arg::F32(60.0), Arg::F32(1.0)]),
            ("/midi/0/cc/1", vec![Arg::F32(0.5)]),
            ("/midi/01/cc/1", vec![Arg::F32(0.5)]),
            ("/midi/1/cc/128", vec![Arg::F32(0.5)]),
            ("/midi/1/bend", vec![Arg::F32(2.0)]),
            ("/midi/1/cc/1", vec![Arg::F32(1.5)]),
            ("/midi/1/cc/1", vec![]),
            ("/midi/1/cc/1", vec![Arg::F32(0.5), Arg::F32(0.5)]),
            ("/midi/1/note", vec![Arg::F32(60.5), Arg::F32(1.0)]),
            ("/midi/1/note", vec![Arg::F32(128.0), Arg::F32(1.0)]),
            ("/midi/1/program", vec![Arg::I32(128)]),
            ("/midi/1/clock", vec![]),
            ("/midi/1/bend/2", vec![Arg::F32(0.0)]),
            ("/fader/in", vec![Arg::F32(0.5)]),
        ] {
            assert_eq!(parse_translated(address, &args), None, "{address} {args:?}");
        }
    }
}

//! OSC-in — decode external OSC/UDP packets into core [`Message`]s.
//!
//! OSC is reuben's lingua franca, so an external packet and an internal Message are the
//! same shape (address + typed args). We map OSC argument types onto [`Arg`], flatten
//! bundles into their contained messages, and stamp `frame = 0`.
//!
//! **External OSC is block-quantized by design.** Reconstructing a sub-block sample
//! position from a UDP datagram's arrival time is pointless: network + scheduler jitter on
//! that arrival (often well over a block) already dwarfs sample resolution, so a `frame`
//! derived from it would be precise-looking noise. Sample-accurate timing is an *internal*
//! property — events generated inside the graph (the Clock and what it drives) sit on the
//! deterministic sample timeline and carry real frames. Bundle timetags are likewise
//! ignored here; explicit musical-time scheduling resolves against the Clock (ADR-0006),
//! not against wall-clock arrival.

use reuben_core::message::{Arg, Message};
use rosc::{OscMessage, OscPacket, OscType};

/// Decode a single UDP datagram of OSC into zero or more Messages.
///
/// A bundle yields one Message per contained message (recursively). Unsupported argument
/// types (blob, nil, time, color, midi, …) are dropped from the arg list.
pub fn decode(bytes: &[u8]) -> Result<Vec<Message>, rosc::OscError> {
    let (_, packet) = rosc::decoder::decode_udp(bytes)?;
    let mut out = Vec::new();
    flatten(packet, &mut out);
    Ok(out)
}

fn flatten(packet: OscPacket, out: &mut Vec<Message>) {
    match packet {
        OscPacket::Message(m) => {
            let args = m.args.iter().filter_map(arg_from_osc).collect::<Vec<_>>();
            out.push(Message::new(m.addr, args, 0));
        }
        OscPacket::Bundle(b) => {
            for content in b.content {
                flatten(content, out);
            }
        }
    }
}

/// Encode an outbound Message (ADR-0026) into a single OSC/UDP datagram — the trivial inverse
/// of [`decode`]. The outbound route is Message-domain, so every [`Arg`] maps to an OSC type;
/// this only errors if the encoder itself rejects the packet. `addr` is the full OSC path (the
/// `osc_out` node's address); `args` are sent verbatim.
pub fn encode(addr: &str, args: &[Arg]) -> Result<Vec<u8>, rosc::OscError> {
    rosc::encoder::encode(&OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args: args.iter().map(arg_to_osc).collect(),
    }))
}

/// Map a core [`Arg`] back onto an OSC argument (inverse of [`arg_from_osc`]). Int → OSC `Int`
/// (i32), the type [`decode`] reads back as [`Arg::Int`]; control-feedback values sit well
/// inside i32, and i32 is the most widely-understood OSC integer (TouchOSC et al.).
fn arg_to_osc(a: &Arg) -> OscType {
    match a {
        Arg::Float(f) => OscType::Float(*f),
        Arg::Int(i) => OscType::Int(*i as i32),
        Arg::Bool(b) => OscType::Bool(*b),
        Arg::Sym(s) => OscType::String(s.clone()),
    }
}

/// Map an OSC argument onto a core [`Arg`], or `None` if unsupported.
fn arg_from_osc(t: &OscType) -> Option<Arg> {
    match t {
        OscType::Int(i) => Some(Arg::Int(*i as i64)),
        OscType::Long(i) => Some(Arg::Int(*i)),
        OscType::Float(f) => Some(Arg::Float(*f)),
        OscType::Double(d) => Some(Arg::Float(*d as f32)),
        OscType::Bool(b) => Some(Arg::Bool(*b)),
        OscType::String(s) => Some(Arg::Sym(s.clone())),
        OscType::Char(c) => Some(Arg::Sym(c.to_string())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosc::{OscBundle, OscMessage, OscTime};

    fn encode(packet: &OscPacket) -> Vec<u8> {
        rosc::encoder::encode(packet).expect("encode")
    }

    #[test]
    fn decodes_a_note_message() {
        let packet = OscPacket::Message(OscMessage {
            addr: "/voicer/note".into(),
            args: vec![OscType::Float(69.0), OscType::Float(1.0)],
        });
        let msgs = decode(&encode(&packet)).expect("decode");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].addr, "/voicer/note");
        assert_eq!(
            msgs[0].args.as_slice(),
            &[Arg::Float(69.0), Arg::Float(1.0)]
        );
        assert_eq!(msgs[0].frame, 0);
    }

    #[test]
    fn maps_int_long_double_bool_string() {
        let packet = OscPacket::Message(OscMessage {
            addr: "/x".into(),
            args: vec![
                OscType::Int(7),
                OscType::Long(9),
                OscType::Double(2.5),
                OscType::Bool(true),
                OscType::String("hi".into()),
            ],
        });
        let msgs = decode(&encode(&packet)).expect("decode");
        assert_eq!(
            msgs[0].args.as_slice(),
            &[
                Arg::Int(7),
                Arg::Int(9),
                Arg::Float(2.5),
                Arg::Bool(true),
                Arg::Sym("hi".into()),
            ]
        );
    }

    #[test]
    fn drops_unsupported_args() {
        let packet = OscPacket::Message(OscMessage {
            addr: "/x".into(),
            args: vec![OscType::Float(1.0), OscType::Nil, OscType::Blob(vec![1, 2])],
        });
        let msgs = decode(&encode(&packet)).expect("decode");
        assert_eq!(msgs[0].args.as_slice(), &[Arg::Float(1.0)]);
    }

    #[test]
    fn encode_round_trips_through_decode() {
        // encode is the inverse of decode for every representable arg (ADR-0026): floats, ints
        // in i32 range, bools, and symbols all survive the boundary out-and-back.
        let addr = "/fb/level";
        let args = [
            Arg::Float(0.5),
            Arg::Int(7),
            Arg::Bool(true),
            Arg::Sym("hi".into()),
        ];
        // `super::encode` — the module's pub fn, not the local `OscPacket` test helper above.
        let bytes = super::encode(addr, &args).expect("encode");
        let msgs = decode(&bytes).expect("decode");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].addr, addr);
        assert_eq!(msgs[0].args.as_slice(), &args);
    }

    #[test]
    fn flattens_a_bundle_into_messages() {
        let packet = OscPacket::Bundle(OscBundle {
            timetag: OscTime {
                seconds: 0,
                fractional: 1,
            },
            content: vec![
                OscPacket::Message(OscMessage {
                    addr: "/a".into(),
                    args: vec![OscType::Int(1)],
                }),
                OscPacket::Message(OscMessage {
                    addr: "/b".into(),
                    args: vec![OscType::Int(2)],
                }),
            ],
        });
        let msgs = decode(&encode(&packet)).expect("decode");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].addr, "/a");
        assert_eq!(msgs[1].addr, "/b");
    }
}

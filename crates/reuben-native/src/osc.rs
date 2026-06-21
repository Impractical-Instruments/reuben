//! OSC-in — decode external OSC/UDP packets into core [`Message`]s.
//!
//! OSC is reuben's lingua franca, so an external packet and an internal Message are the
//! same shape (address + typed args). We map OSC argument types onto [`Arg`], flatten
//! bundles into their contained messages, and stamp `frame = 0` (block-quantized; the
//! bundle timetag is ignored until musical-time scheduling lands).

use reuben_core::message::{Arg, Message};
use rosc::{OscPacket, OscType};

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

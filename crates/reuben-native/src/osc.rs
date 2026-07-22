//! OSC codec — decode external OSC/UDP datagrams into the **flat primitive form**, and encode
//! outbound Messages back out.
//!
//! OSC is reuben's lingua franca, but an internal [`Message`](reuben_core::message::Message)
//! carries exactly **one** [`Arg`], whereas an OSC message is a flat list of args. So this layer
//! stays *untyped*: [`decode`] yields each datagram's address plus a flat `Vec<Arg>` of OSC
//! **primitives** ([`Arg::F32`]/[`Arg::I32`]/[`Arg::Str`]); turning that flat list into the single
//! typed `Arg` a destination port carries is **dest-port-type-driven** and lives in
//! [`reuben_core::boundary::osc_in_arg`] — driven by the descriptor, applied where the Plan is
//! known (the engine), not here. [`encode`] is the inverse: it takes the already-flattened OSC
//! args (produced by [`reuben_core::boundary::osc_out_args`]) and packs one datagram.
//!
//! **External OSC is block-quantized by design.** Reconstructing a sub-block sample position from a
//! UDP datagram's arrival time is pointless: network + scheduler jitter on that arrival (often well
//! over a block) already dwarfs sample resolution, so a `frame` derived from it would be precise-
//! looking noise. Sample-accurate timing is an *internal* property — events generated inside the
//! graph (the Clock and what it drives) sit on the deterministic sample timeline. Incoming OSC is
//! stamped `frame = 0` ("now") when the engine builds Messages. Bundle timetags are likewise
//! ignored; explicit musical-time scheduling resolves against the Clock, not wall-clock.
//!
//! see rules: signal-time-dsp

use reuben_core::message::Arg;
use rosc::{OscMessage, OscPacket, OscType};

/// The UDP port `reuben play` binds for OSC-in — the engine's **foreign edge**, where external
/// controllers (a hardware knob, a TouchOSC surface) reach it. Bound on `0.0.0.0` (all interfaces),
/// unlike the loopback-only structure channel.
///
/// It lives here, in the OSC codec, because this module *is* that edge and `play` is its only
/// consumer. It used to live beside `DEFAULT_STRUCTURE_ADDR` in `reuben_core`'s wire envelope, back
/// when the reuben-mcp sidecar dialed it to deliver `send` — two ends that had to agree on one
/// literal. The sidecar's control now rides the structure channel, so there is no second end left
/// to drift from, and core carries no network plumbing.
pub const DEFAULT_OSC_PORT: u16 = 9000;

/// One inbound control message in **flat primitive form**: an address plus its args as primitive
/// [`Arg`]s, *before* dest-port-type-driven conversion to the single typed `Arg`. The engine routes
/// `address` to a node/port and calls [`reuben_core::boundary::osc_in_arg`] with that port's type
/// to produce the Message.
///
/// **Two producers feed this, not one.** [`decode`] mints them from external UDP datagrams (the
/// foreign edge), and the structure channel's `send` verb mints them from its own NDJSON framing
/// (`crate::structure`) — every door ships `{address, [Arg]}` in its own framing and converges
/// here, on one `mpsc` into the render callback's `queue_osc`. So this type is the flat carrier,
/// not an OSC-specific one; it lives in this module because decoding is where most of them are
/// born.
#[derive(Debug, Clone, PartialEq)]
pub struct OscIn {
    /// OSC address path, e.g. `/voicer/notes`.
    pub address: String,
    /// The OSC args as primitive `Arg`s (`F32`/`I32`/`Str`), in order.
    pub args: Vec<Arg>,
}

/// Decode a single UDP datagram of OSC into zero or more flat [`OscIn`]s.
///
/// A bundle yields one `OscIn` per contained message (recursively). Unsupported argument types
/// (blob, nil, time, color, midi, …) are dropped from the arg list.
pub fn decode(bytes: &[u8]) -> Result<Vec<OscIn>, rosc::OscError> {
    let (_, packet) = rosc::decoder::decode_udp(bytes)?;
    let mut out = Vec::new();
    flatten(packet, &mut out);
    Ok(out)
}

fn flatten(packet: OscPacket, out: &mut Vec<OscIn>) {
    match packet {
        OscPacket::Message(m) => {
            let args = m.args.iter().filter_map(arg_from_osc).collect::<Vec<_>>();
            out.push(OscIn {
                address: m.addr,
                args,
            });
        }
        OscPacket::Bundle(b) => {
            for content in b.content {
                flatten(content, out);
            }
        }
    }
}

/// Encode an outbound datagram: a full OSC address plus the **flat OSC args** already
/// expanded by [`reuben_core::boundary::osc_out_args`]. Every `Arg` here is a primitive; a non-
/// primitive (it should never reach this point) is dropped. Errors only if the encoder itself
/// rejects the packet.
pub fn encode(addr: &str, args: &[Arg]) -> Result<Vec<u8>, rosc::OscError> {
    rosc::encoder::encode(&OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args: args.iter().filter_map(arg_to_osc).collect(),
    }))
}

/// Map a primitive core [`Arg`] back onto an OSC argument (inverse of [`arg_from_osc`]). `I32` →
/// OSC `Int` (i32), the type [`decode`] reads back as [`Arg::I32`]; control-feedback values sit
/// well inside i32, and i32 is the most widely-understood OSC integer (TouchOSC et al.). A non-
/// primitive `Arg` has no OSC atom and yields `None` (the boundary expansion never produces one).
fn arg_to_osc(a: &Arg) -> Option<OscType> {
    match a {
        Arg::F32(f) => Some(OscType::Float(*f)),
        Arg::I32(i) => Some(OscType::Int(*i)),
        Arg::Str(s) => Some(OscType::String(s.to_string())),
        _ => None,
    }
}

/// Map an OSC argument onto a primitive core [`Arg`], or `None` if unsupported. OSC has no carrier
/// for the typed vocab forms — those are reconstructed from these primitives at the boundary
/// ([`reuben_core::boundary::osc_in_arg`]). A `Bool` maps to `I32` 0/1.
fn arg_from_osc(t: &OscType) -> Option<Arg> {
    match t {
        OscType::Int(i) => Some(Arg::I32(*i)),
        OscType::Long(i) => Some(Arg::I32(*i as i32)),
        OscType::Float(f) => Some(Arg::F32(*f)),
        OscType::Double(d) => Some(Arg::F32(*d as f32)),
        OscType::Bool(b) => Some(Arg::I32(*b as i32)),
        OscType::String(s) => Some(Arg::Str(s.as_str().into())),
        OscType::Char(c) => Some(Arg::Str(c.to_string().into())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosc::{OscBundle, OscMessage, OscTime};

    fn encode_packet(packet: &OscPacket) -> Vec<u8> {
        rosc::encoder::encode(packet).expect("encode")
    }

    #[test]
    fn decodes_a_note_message_to_flat_args() {
        let packet = OscPacket::Message(OscMessage {
            addr: "/voicer/notes".into(),
            args: vec![OscType::Float(69.0), OscType::Float(1.0)],
        });
        let msgs = decode(&encode_packet(&packet)).expect("decode");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].address, "/voicer/notes");
        assert_eq!(msgs[0].args, vec![Arg::F32(69.0), Arg::F32(1.0)]);
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
        let msgs = decode(&encode_packet(&packet)).expect("decode");
        assert_eq!(
            msgs[0].args,
            vec![
                Arg::I32(7),
                Arg::I32(9),
                Arg::F32(2.5),
                Arg::I32(1),
                Arg::Str("hi".into()),
            ]
        );
    }

    #[test]
    fn drops_unsupported_args() {
        let packet = OscPacket::Message(OscMessage {
            addr: "/x".into(),
            args: vec![OscType::Float(1.0), OscType::Nil, OscType::Blob(vec![1, 2])],
        });
        let msgs = decode(&encode_packet(&packet)).expect("decode");
        assert_eq!(msgs[0].args, vec![Arg::F32(1.0)]);
    }

    #[test]
    fn encode_round_trips_through_decode() {
        // encode is the inverse of decode for every primitive arg: floats, ints in i32
        // range, and symbols all survive the boundary out-and-back.
        let addr = "/fb/level";
        let args = [Arg::F32(0.5), Arg::I32(7), Arg::Str("hi".into())];
        let bytes = encode(addr, &args).expect("encode");
        let msgs = decode(&bytes).expect("decode");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].address, addr);
        assert_eq!(msgs[0].args, args);
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
        let msgs = decode(&encode_packet(&packet)).expect("decode");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].address, "/a");
        assert_eq!(msgs[1].address, "/b");
    }
}

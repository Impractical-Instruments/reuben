//! `osc_out` — the boundary sink that sends Messages out over OSC (ADR-0026).
//!
//! The outward half of reuben's OSC I/O and the mirror of OSC-in (ADR-0007): core stays
//! OSC-agnostic, so this op never encodes or touches UDP. It collects its input Messages onto
//! the **outbound route** — the fourth lane, modelled on the context lane's publish mechanics
//! (ADR-0015) — and native drains that route each block, encodes, and sends to the static
//! `--osc-out host:port` target.
//!
//! **Address.** The node's address *is* the outbound OSC address (one sink = one address): the
//! engine stamps it on drain. The op forwards only the args; the incoming event's local address
//! (an internal emit label like `out`/`degree`) is dropped, not leaked onto the wire.
//!
//! **Message-domain only** (ADR-0026). A Good Button's `map` output is already a Message, so
//! two-way control-surface feedback works without new machinery. Sending a live Signal value out
//! needs the deferred Signal→Message sampler (ADR-0017); v1 OSC-out does not.
//!
//! - input 0: `in` (`Note` event) — values to send out. In the unified model (ADR-0030) the sink
//!   simply **emits** each received Message; the engine's outbound tap (`Plan.outbound_taps`)
//!   drains an `osc_out` node's emissions past the boundary, where the flat OSC form is encoded.
//!   The incoming event's local address is dropped; the node's address is stamped on drain.

use smallvec::SmallVec;

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};
use crate::pitch::Note;

// Single-source contract (ADR-0025/0030): one declaration -> IN_/OUT_ consts + Descriptor, no drift.
// `OscOut` -> type_name "osc_out" (snake_case, required by the contract validator — the wire name
// is `osc_out`, not the ADR's prose `osc-out`; the *CLI flag* keeps the hyphen).
crate::operator_contract!(OscOut {
    inputs: { in: note },
});

#[derive(Default)]
pub struct OscOut;

impl OscOut {
    pub fn new() -> Self {
        Self
    }
}

impl Operator for OscOut {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        // Snapshot first: `io.stream` borrows immutably and `io.emit` needs `&mut io`, so they
        // can't overlap. Inline storage — no heap for the common handful of events per block.
        // Each received Message is re-emitted; the engine's outbound tap drains these past the
        // boundary (ADR-0030). The frame is segment-relative; `emit` does not add the offset here
        // because the stream frames are already segment-relative and the tap stamps block-absolute.
        let mut pending: SmallVec<[(Note, usize); 4]> = SmallVec::new();
        for ev in io.stream::<Note>(IN_IN) {
            pending.push((ev.payload, ev.frame));
        }
        for (note, frame) in pending {
            io.emit(0, "out", note, frame);
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(OscOut);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Arg, Emit, Event, Message};
    use crate::pitch::Pitch;

    const SR: f32 = 48_000.0;

    /// Run one block; return the emissions the sink produced. In the unified model the engine's
    /// outbound tap drains these past the boundary, so a unit test captures them via the emit sink.
    fn run(op: &mut OscOut, n: usize, events: &[Message]) -> Vec<Emit> {
        let evs: Vec<Event> = events
            .iter()
            .map(|m| Event {
                address: &m.address,
                arg: &m.arg,
                frame: m.frame,
            })
            .collect();
        let streams: [&[Event]; 1] = [&evs[..]];
        let mut emits: Vec<Emit> = Vec::new();
        {
            let outs: Vec<&mut [f32]> = vec![];
            let inputs: Vec<Option<&[f32]>> = vec![None];
            let mut io = Io::new(SR, n, inputs, outs)
                .with_streams(&streams)
                .with_emit(&mut emits, 0);
            op.process(&mut io);
        }
        emits
    }

    fn note(addr: &str, degree: i32, frame: usize) -> Message {
        Message::new(addr, Note::new(Pitch::Degree(degree), 1.0), frame)
    }

    #[test]
    fn forwards_each_input_event_to_the_outbound_route() {
        let mut op = OscOut::new();
        let evs = [note("anything", 7, 10), note("ignored_local_addr", 12, 20)];
        let out = run(&mut op, 128, &evs);
        assert_eq!(out.len(), 2, "one emission per input event");
        // Payload forwarded verbatim; the event's local address is dropped (stamped later).
        assert_eq!(out[0].arg, Arg::Note(Note::new(Pitch::Degree(7), 1.0)));
        assert_eq!(out[0].frame, 10);
        assert_eq!(out[1].arg, Arg::Note(Note::new(Pitch::Degree(12), 1.0)));
        assert_eq!(out[1].frame, 20);
    }

    #[test]
    fn no_events_sends_nothing() {
        let mut op = OscOut::new();
        assert!(run(&mut op, 128, &[]).is_empty());
    }
}

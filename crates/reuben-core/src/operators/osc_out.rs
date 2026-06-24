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
//! - input 0: `in` (Message) — values to send out (any address; args forwarded verbatim).

use smallvec::SmallVec;

use crate::descriptor::Descriptor;
use crate::message::Args;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025): one declaration -> IN_/OUT_/P_ consts + Descriptor, no drift.
// `OscOut` -> type_name "osc_out" (snake_case, required by the contract validator — the wire name
// is `osc_out`, not the ADR's prose `osc-out`; the *CLI flag* keeps the hyphen).
crate::operator_contract!(OscOut {
    inputs: { in: message },
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
        // Snapshot the inputs first: `io.events()` borrows immutably and `send_outbound` needs
        // `&mut io`, so they can't overlap (the same constraint the context node works around).
        // Inline storage — no heap for the common handful of events per block.
        let mut pending: SmallVec<[(Args, usize); 4]> = SmallVec::new();
        for ev in io.events() {
            pending.push((ev.args.clone(), ev.frame));
        }
        for (args, frame) in pending {
            io.send_outbound(args, frame);
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
    use crate::message::{Arg, Event, Message, Outbound};

    const SR: f32 = 48_000.0;

    /// Run one block; return the outbound Messages the sink collected (block-absolute frames).
    fn run(op: &mut OscOut, n: usize, events: &[Message]) -> Vec<Outbound> {
        let evs: Vec<Event> = events
            .iter()
            .map(|m| Event {
                addr: &m.addr,
                args: &m.args,
                frame: m.frame,
            })
            .collect();
        let mut out: Vec<Outbound> = Vec::new();
        {
            let outs: Vec<&mut [f32]> = vec![];
            let inputs: Vec<Option<&[f32]>> = vec![None];
            let params: Vec<f32> = vec![];
            let mut io = Io::new(SR, n, inputs, outs, &params, &evs).with_outbound(&mut out, 0);
            op.process(&mut io);
        }
        out
    }

    #[test]
    fn forwards_each_input_event_to_the_outbound_route() {
        let mut op = OscOut::new();
        let evs = [
            Message::new("anything", [Arg::Float(0.7)], 10),
            Message::new("ignored_local_addr", [Arg::Int(42), Arg::Bool(true)], 20),
        ];
        let out = run(&mut op, 128, &evs);
        assert_eq!(out.len(), 2, "one outbound Message per input event");
        // Args forwarded verbatim; the event's local address is dropped (stamped later).
        assert_eq!(out[0].args.as_slice(), &[Arg::Float(0.7)]);
        assert_eq!(out[0].frame, 10);
        assert_eq!(out[1].args.as_slice(), &[Arg::Int(42), Arg::Bool(true)]);
        assert_eq!(out[1].frame, 20);
    }

    #[test]
    fn no_events_sends_nothing() {
        let mut op = OscOut::new();
        assert!(run(&mut op, 128, &[]).is_empty());
    }
}

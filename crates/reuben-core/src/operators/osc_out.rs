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
//! - input 0: `in` (`arg` — the type-agnostic pass-through, issue #141) — values to send out. The
//!   sink forwards **any** [`Arg`] verbatim: a `Note`, a scalar echo, a vocab enum, a string —
//!   whatever Message-domain source is wired in. The type-driven expansion to the flat OSC form
//!   happens past the boundary ([`osc_out_args`](crate::boundary::osc_out_args)); an Arg with no
//!   external form (`Harmony`) is dropped there, and a Signal source is rejected at plan time. In
//!   the unified model (ADR-0030) the sink simply **emits** each received Message; the engine's
//!   outbound tap (`Plan.outbound_taps`) drains an `osc_out` node's emissions past the boundary,
//!   where the flat OSC form is encoded. The incoming event's local address is dropped; the node's
//!   address is stamped on drain.

use smallvec::SmallVec;

use crate::descriptor::Descriptor;
use crate::message::Arg;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025/0030): one declaration -> IN_/OUT_ consts + Descriptor, no drift.
// `OscOut` -> type_name "osc_out" (snake_case, required by the contract validator — the wire name
// is `osc_out`, not the ADR's prose `osc-out`; the *CLI flag* keeps the hyphen).
crate::operator_contract!(OscOut {
    inputs: { in: arg },
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
        // Snapshot first: `io.input` borrows immutably and `io.output` needs `&mut io`, so they
        // can't overlap. Inline storage — no heap for the common handful of events per block.
        // Each received Message is re-emitted verbatim and addressless — the raw `Arg`, no vocab
        // decode (issue #141) — so the boundary's type-driven expansion sees exactly what arrived.
        // The engine's outbound tap stamps the node's OSC address and drains these past the
        // boundary (ADR-0030, ADR-0031). Cloning an `Arg` is alloc-free for every hot-path payload
        // (`Str` aside, which only appears on cold paths). `frame` is segment-relative; the writer
        // adds the segment offset so the tap sees block-absolute frames.
        let mut pending: SmallVec<[(Arg, usize); 4]> = SmallVec::new();
        for ev in io.input::<&Arg>(IN_IN) {
            pending.push((ev.payload.clone(), ev.frame));
        }
        for (arg, frame) in pending {
            io.output::<Arg>(0).emit(frame, arg);
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
    use crate::op_driver::OpDriver;
    use crate::vocab::pitch::{Note, Pitch};
    use crate::vocab::FilterMode;

    const SR: f32 = 48_000.0;

    /// A degree-note `Note` event for the `in` port (the sink drops the incoming local address, so
    /// only the payload + frame are observable downstream).
    fn note(degree: i32, frame: usize) -> (usize, Note) {
        (frame, Note::new(Pitch::Degree(degree), 1.0))
    }

    #[test]
    fn forwards_each_input_event_to_the_outbound_route() {
        // In the unified model the engine's outbound tap drains the sink's emissions past the
        // boundary, so a unit test captures them via `emits()`.
        let mut d = OpDriver::for_type(OscOut::new(), SR);
        for (frame, n) in [note(7, 10), note(12, 20)] {
            d.push(IN_IN, frame, n);
        }
        d.render(128);
        let out = d.emits();
        assert_eq!(out.len(), 2, "one emission per input event");
        // Payload forwarded verbatim; the event's local address is dropped (stamped later).
        assert_eq!(out[0].arg, Arg::Note(Note::new(Pitch::Degree(7), 1.0)));
        assert_eq!(out[0].frame, 10);
        assert_eq!(out[1].arg, Arg::Note(Note::new(Pitch::Degree(12), 1.0)));
        assert_eq!(out[1].frame, 20);
    }

    /// The sink is type-agnostic (issue #141): any `Arg` family — a scalar, a string, a
    /// type-erased vocab enum — forwards verbatim, not just `Note`. This is what lets vocab enums
    /// and control-value echoes reach the outbound boundary at all.
    #[test]
    fn forwards_any_arg_type_verbatim() {
        let mut d = OpDriver::for_type(OscOut::new(), SR);
        d.push(IN_IN, 5, Arg::F32(0.25));
        d.push(IN_IN, 10, Arg::from(FilterMode::Bp));
        d.push(IN_IN, 15, Arg::Str("Up".into()));
        d.render(128);
        let out = d.emits();
        assert_eq!(out.len(), 3, "one emission per input event");
        assert_eq!((out[0].frame, &out[0].arg), (5, &Arg::F32(0.25)));
        assert_eq!(
            (out[1].frame, &out[1].arg),
            (10, &Arg::from(FilterMode::Bp))
        );
        assert_eq!((out[2].frame, &out[2].arg), (15, &Arg::Str("Up".into())));
    }

    #[test]
    fn no_events_sends_nothing() {
        let mut d = OpDriver::for_type(OscOut::new(), SR);
        assert!(d.render(128).emits().is_empty());
    }
}

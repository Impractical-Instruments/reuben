//! Message — the discrete, OSC-shaped payload that flows on edges and crosses the
//! boundary (ADR-0001, ADR-0007).
//!
//! An internal Message and an external OSC packet are the same shape: an address path,
//! typed args, and a sample-accurate timetag. For the "first sound" run the timetag is a
//! sample offset within the current block (`frame`); musical-time timetags resolved
//! against the Clock land later (ADR-0006).

use smallvec::SmallVec;

/// A typed OSC argument.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Float(f32),
    Int(i64),
    Bool(bool),
    /// An interned symbol / string atom.
    Sym(String),
}

impl Arg {
    /// Best-effort numeric view, for params that accept a number.
    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Arg::Float(v) => Some(*v),
            Arg::Int(v) => Some(*v as f32),
            Arg::Bool(v) => Some(if *v { 1.0 } else { 0.0 }),
            Arg::Sym(_) => None,
        }
    }
}

/// Inline storage for the common small-arg case; spills to the heap beyond it.
pub type Args = SmallVec<[Arg; 4]>;

/// A discrete, addressed, sample-accurate payload.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// OSC-style address path, e.g. `/osc/freq` or `/note`.
    pub addr: String,
    /// Typed arguments.
    pub args: Args,
    /// Sample offset within the current Render block at which this Message applies.
    pub frame: usize,
}

impl Message {
    pub fn new(addr: impl Into<String>, args: impl IntoIterator<Item = Arg>, frame: usize) -> Self {
        Self {
            addr: addr.into(),
            args: args.into_iter().collect(),
            frame,
        }
    }

    /// Convenience: a single-float Message (the common param-change case).
    pub fn float(addr: impl Into<String>, value: f32, frame: usize) -> Self {
        Self::new(addr, [Arg::Float(value)], frame)
    }

    /// First arg as f32, if present and numeric.
    pub fn first_f32(&self) -> Option<f32> {
        self.args.first().and_then(Arg::as_f32)
    }
}

/// A Message an operator emits during `process` onto a Message output port (ADR-0014),
/// before the engine stamps it block-absolute and routes it to downstream nodes' event
/// lists. Distinct from the boundary [`Message`]: its address is a `&'static str` and its
/// args are inline, so emitting a note on the wired hot path touches no allocator.
#[derive(Debug, Clone)]
pub struct Emit {
    /// Which Message output port it went to, as an ordinal among the operator's Message
    /// outputs (a separate index space from Signal outputs).
    pub port: usize,
    /// Node-local address the destination matches in [`Io::events`](crate::operator::Io::events),
    /// e.g. `"note"`. Static — the wired edge, not this string, is the routing.
    pub addr: &'static str,
    /// Typed arguments.
    pub args: Args,
    /// Sample offset within the Render block. Segment-relative when the operator calls
    /// `emit`; the engine stamps it block-absolute.
    pub frame: usize,
}

/// A boundary-bound Message an `osc_out` sink collects during `process` onto the **outbound
/// route** (ADR-0026) — the fourth lane, modelled on the context lane's publish mechanics
/// (ADR-0015). The engine stamps it block-absolute and with the node's address (the outbound
/// OSC address), then drains it past the boundary for native to encode + UDP-send. Carries no
/// address of its own: the sink is address-fixed, so the node *is* the routing, not this payload.
#[derive(Debug, Clone)]
pub struct Outbound {
    /// Typed arguments to send out.
    pub args: Args,
    /// Sample offset within the Render block. Segment-relative when the operator calls
    /// `send_outbound`; the engine stamps it block-absolute.
    pub frame: usize,
}

/// A routed event handed to an event operator for one (sub)block, via
/// [`crate::operator::Io::events`]. A zero-copy view onto the originating block
/// [`Message`]: the address *local* to the receiving node, the typed args, and a
/// segment-relative frame. The Render loop builds these in place (no allocation),
/// which is what keeps Render realtime-safe even while delivering events.
#[derive(Debug, Clone, Copy)]
pub struct Event<'a> {
    /// Address local to the receiving node, e.g. `note` for `/voicer/note` under `/voicer`.
    pub addr: &'a str,
    /// Typed arguments, borrowed from the source Message.
    pub args: &'a Args,
    /// Sample offset within the current (sub)block at which this event applies.
    pub frame: usize,
}

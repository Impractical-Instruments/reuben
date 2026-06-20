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

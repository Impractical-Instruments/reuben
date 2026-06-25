//! Message — the one OSC-shaped carrier the core speaks (ADR-0007, ADR-0030).
//!
//! A **Message** is `address + timestamp + exactly one Arg`. It is close to the OSC spec
//! without its binary representation, with three deliberate divergences (ADR-0030):
//!
//! - an internal **timestamp** (`frame`, a sample offset within the current Render block),
//!   which OSC Messages lack — incoming external OSC is stamped "now" (frame 0);
//! - **exactly one [`Arg`]**, not many — which is *why* concrete-type Args exist: two scalars
//!   (a note's pitch + velocity) cannot be two args, so they pack into one `Arg::Note`;
//! - **concrete-type Args** instead of OSC's primitives-or-blob — human-readable and a
//!   compile-time data contract.
//!
//! Everything the core used to carry on seven separate lanes — dense audio, sparse events,
//! the harmony struct, held enums, params, materialized floats, the outbound sink — is one
//! Message stream read three ways: as a stream of events, as a held (zero-order-hold) value,
//! or, for a [`Buffer`](Arg::Buffer) payload, as a dense per-sample block.

use crate::harmony::Harmony;
use crate::pitch::Note;

/// A contiguous sample buffer — the performant representation of a per-sample stream (a
/// "Signal", ADR-0001, ADR-0030). `Signal<f32>` is the only element kind built today; the
/// type parameter exists so other kinds can land later without minting a second type, and is
/// deliberately not generalized further now.
///
/// On the hot path the engine keeps buffers in its per-edge arena and hands operators
/// borrowed `&[f32]` / `&mut [f32]` (zero-copy across an edge, disjoint within a node). This
/// owned form is the conceptual / boundary representation that completes the [`Arg`] enum.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Signal<T = f32> {
    samples: Vec<T>,
}

impl<T> Signal<T> {
    /// Wrap an owned sample vector.
    pub fn from_vec(samples: Vec<T>) -> Self {
        Self { samples }
    }

    /// The samples, as a slice.
    pub fn as_slice(&self) -> &[T] {
        &self.samples
    }

    /// The samples, mutably.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.samples
    }

    /// Number of samples.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

/// The single typed payload of a [`Message`] (ADR-0030).
///
/// A **closed, central** enum with three families:
/// - **OSC primitives** — [`F32`](Arg::F32) / [`I32`](Arg::I32) / [`Str`](Arg::Str);
/// - **shared *vocab* concrete types** — defined once and reused everywhere (a `FilterMode`
///   duplicated per-operator would be the smell), which is what lets a *closed* enum
///   enumerate them. [`Note`](Arg::Note) and [`Harmony`](Arg::Harmony) are here today; phase 2
///   adds the `vocab` module + `ArgValue` derive that folds in the operator enums
///   (`FilterMode`, `Waveform`, `SnapMode`, `MapCurve`, `M2sMode`, …), each generating its own
///   OSC conversion + metadata;
/// - the optimized dense payload — [`Buffer`](Arg::Buffer), a [`Signal`]'s samples.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    // OSC primitives.
    F32(f32),
    I32(i32),
    /// A string / symbol atom. Cold/boundary paths only (interned later if it shows hot).
    Str(String),

    // Shared vocab concrete types (more land in phase 2).
    Note(Note),
    Harmony(Harmony),

    // The optimized dense payload.
    Buffer(Signal<f32>),
}

impl Arg {
    /// Best-effort scalar view, for a port that accepts a number. Only the numeric primitives
    /// answer; vocab types and buffers return `None` (they decode through their own typed read).
    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Arg::F32(v) => Some(*v),
            Arg::I32(v) => Some(*v as f32),
            _ => None,
        }
    }

    /// The held buffer, if this Arg is a [`Buffer`](Arg::Buffer).
    pub fn as_buffer(&self) -> Option<&Signal<f32>> {
        match self {
            Arg::Buffer(b) => Some(b),
            _ => None,
        }
    }
}

impl From<f32> for Arg {
    fn from(v: f32) -> Self {
        Arg::F32(v)
    }
}

impl From<i32> for Arg {
    fn from(v: i32) -> Self {
        Arg::I32(v)
    }
}

impl From<Note> for Arg {
    fn from(v: Note) -> Self {
        Arg::Note(v)
    }
}

impl From<Harmony> for Arg {
    fn from(v: Harmony) -> Self {
        Arg::Harmony(v)
    }
}

/// A discrete, addressed, sample-accurate payload — the boundary/owned form of the core's one
/// carrier (ADR-0007, ADR-0030).
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// OSC-style address path, e.g. `/osc/freq` or `/note`. Kept for OSC shape, boundary
    /// routing, and debug — **never** internal dispatch (that is the wired edge's job).
    pub address: String,
    /// Sample offset within the current Render block at which this Message applies. The
    /// internal timestamp OSC lacks; incoming external OSC is stamped 0 ("now").
    pub frame: usize,
    /// The single typed payload.
    pub arg: Arg,
}

impl Message {
    /// Build a Message from its address, payload, and frame.
    pub fn new(address: impl Into<String>, arg: impl Into<Arg>, frame: usize) -> Self {
        Self {
            address: address.into(),
            frame,
            arg: arg.into(),
        }
    }

    /// Convenience: a single-float Message (the common param-change case).
    pub fn float(address: impl Into<String>, value: f32, frame: usize) -> Self {
        Self::new(address, Arg::F32(value), frame)
    }

    /// The payload as f32, if it is a numeric primitive.
    pub fn as_f32(&self) -> Option<f32> {
        self.arg.as_f32()
    }
}

/// A Message an operator emits during `process` onto a Message output port (ADR-0014),
/// before the engine stamps it block-absolute and routes it to downstream nodes. Distinct
/// from the boundary [`Message`]: its address is a `&'static str` and its payload is the one
/// inline [`Arg`], so emitting on the wired hot path touches no allocator (sparse `Arg::Str`
/// aside, which only appears on cold paths).
#[derive(Debug, Clone)]
pub struct Emit {
    /// Which Message output port it went to, as an ordinal among the operator's Message
    /// outputs (a separate index space from Buffer/Signal outputs).
    pub port: usize,
    /// Node-local address the engine carries for OSC shape / debug, e.g. `"note"`. Static —
    /// the wired edge, not this string, is the routing.
    pub address: &'static str,
    /// The single typed payload.
    pub arg: Arg,
    /// Sample offset within the Render block. Segment-relative when the operator calls `emit`;
    /// the engine stamps it block-absolute.
    pub frame: usize,
}

/// A boundary-bound Message an `osc_out` sink collects during `process` onto the outbound
/// route (ADR-0026). The engine stamps it block-absolute and with the node's (fixed) outbound
/// address, then drains it past the boundary for native to encode + send. Carries no address
/// of its own: the sink is address-fixed, so the node *is* the routing. The single [`Arg`] is
/// expanded to OSC's flat multi-arg form at the boundary (phase 6).
#[derive(Debug, Clone)]
pub struct Outbound {
    /// The single typed payload to send out.
    pub arg: Arg,
    /// Sample offset within the Render block. Segment-relative when the operator calls
    /// `send_outbound`; the engine stamps it block-absolute.
    pub frame: usize,
}

/// A routed event handed to an operator for one (sub)block. A zero-copy view onto the
/// originating block [`Message`]: the address *local* to the receiving node, a borrowed
/// reference to the one [`Arg`], and a segment-relative frame. The Render loop builds these in
/// place (no allocation), keeping Render realtime-safe while delivering events.
///
/// This is the raw delivered form; the typed read API (`io.stream::<T>` / `io.last::<T>`,
/// phase 4) decodes the borrowed `Arg` into the operator's requested payload type.
#[derive(Debug, Clone, Copy)]
pub struct Event<'a> {
    /// Address local to the receiving node, e.g. `note` for `/voicer/note` under `/voicer`.
    pub address: &'a str,
    /// The single typed payload, borrowed from the source Message.
    pub arg: &'a Arg,
    /// Sample offset within the current (sub)block at which this event applies.
    pub frame: usize,
}

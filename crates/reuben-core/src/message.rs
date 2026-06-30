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
//! or, for a [`Buffer`](Arg::F32Buffer) payload, as a dense per-sample block.

use crate::vocab::harmony::Harmony;
use crate::vocab::pitch::Note;

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
/// - **shared *vocab* types** — defined once and reused everywhere (a `FilterMode` duplicated
///   per-operator would be the smell). Each [`vocab`](crate::vocab) type's `#[derive(ArgValue)]`
///   folds it in: a **struct** with a real per-type shape gets its own variant
///   ([`Note`](Arg::Note), [`Harmony`](Arg::Harmony)); every **enum** type-erases to the single
///   [`Enum`](Arg::Enum) index variant, its identity carried by the port (ADR-0030), so adding an
///   enum grows neither this enum nor any other central site;
/// - the optimized dense payload — [`Buffer`](Arg::F32Buffer), a [`Signal`]'s samples.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    // OSC primitives.
    F32(f32),
    I32(i32),
    /// A string / symbol atom. Cold/boundary paths only (interned later if it shows hot).
    Str(String),

    // Shared vocab concrete types (ADR-0030) — each defined once with `#[derive(ArgValue)]`,
    // which generates this variant's `From`/`TryFrom` glue. More land as operators migrate.
    Note(Note),
    Harmony(Harmony),

    /// Any **vocab enum** value, type-erased to its bare variant **index** (ADR-0030). One
    /// variant for *every* enum: type identity lives in the port descriptor's
    /// [`EnumMeta`](crate::descriptor::EnumMeta), never in the value — so adding an enum touches
    /// no central engine site. The operator names the concrete type at the read
    /// (`io.input::<FilterMode>()` → `FilterMode::from_index`), and port-authority guarantees a
    /// latch slot only ever holds its own port's enum, so a bare index cannot mis-decode.
    Enum(u32),

    // The optimized dense payload.
    F32Buffer(Signal<f32>),
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

    /// The held buffer, if this Arg is a [`Buffer`](Arg::F32Buffer).
    pub fn as_f32_buffer(&self) -> Option<&Signal<f32>> {
        match self {
            Arg::F32Buffer(b) => Some(b),
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

// `From<Note>`/`From<Harmony>` (and the other vocab `From`/`TryFrom`) are generated by
// `#[derive(ArgValue)]` on each vocab type (ADR-0030); they are not written here.

/// Decode a borrowed [`Arg`] into an operator's requested payload type — the read side of the
/// typed I/O API (`io.input::<T>`, ADR-0030). One trait spans every payload an
/// operator reads: the OSC primitives (`f32`/`i32`/`&str`), the dense [`Buffer`](Arg::F32Buffer) as a
/// borrowed `&[f32]`, and the shared *vocab* concrete types (whose impl `#[derive(ArgValue)]`
/// generates, delegating to their `TryFrom<&Arg>`).
///
/// The `'a` lifetime lets a payload **borrow** from the Arg (a `&'a str`, a `&'a [f32]`) so a
/// per-sample buffer read is zero-copy on the audio thread; `Copy` payloads (`f32`, `Note`) ignore
/// it. Returns `None` when the Arg is the wrong family (a wrong-typed wire — caught at load, so the
/// render path treats `None` as "absent").
pub trait FromArg<'a>: Sized {
    fn from_arg(arg: &'a Arg) -> Option<Self>;
}

impl<'a> FromArg<'a> for f32 {
    fn from_arg(arg: &'a Arg) -> Option<Self> {
        arg.as_f32()
    }
}

impl<'a> FromArg<'a> for i32 {
    fn from_arg(arg: &'a Arg) -> Option<Self> {
        match arg {
            Arg::I32(v) => Some(*v),
            Arg::F32(v) => Some(v.round() as i32),
            _ => None,
        }
    }
}

impl<'a> FromArg<'a> for &'a str {
    fn from_arg(arg: &'a Arg) -> Option<Self> {
        match arg {
            Arg::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

impl<'a> FromArg<'a> for &'a [f32] {
    fn from_arg(arg: &'a Arg) -> Option<Self> {
        match arg {
            Arg::F32Buffer(b) => Some(b.as_slice()),
            _ => None,
        }
    }
}

/// Pack/unpack a vocab type across the **flat multi-arg OSC form** at the boundary (ADR-0030).
///
/// Internally a Message carries exactly one [`Arg`], but external OSC is a flat list of
/// primitive args — so a struct vocab type spans several (`Note ↔ /note pitch vel`). A type with
/// an external OSC form implements this; **not** implementing it is the boundary opt-out (a
/// [`Buffer`](Arg::F32Buffer) never crosses, so audio is kept off the wire by construction). The
/// `args` read and `out` written are primitive `Arg`s (`F32`/`I32`/`Str`) — the OSC atoms.
///
/// Enums need no impl: a vocab enum is a single OSC arg (a symbol or index), handled by its
/// [`EnumMeta`](crate::descriptor::EnumMeta) resolver. Only multi-arg struct types (`Note`) impl
/// this. The dest-port-type-driven dispatch lives in [`crate::boundary`].
pub trait OscArg: Sized {
    /// Build the type from a flat OSC arg list, or `None` if the args don't fit.
    fn from_osc(args: &[Arg]) -> Option<Self>;
    /// Append this value's flat OSC args (primitive `Arg`s) to `out`.
    fn to_osc(&self, out: &mut Vec<Arg>);
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
/// from the boundary [`Message`]: its payload is the one inline [`Arg`], so emitting on the wired
/// hot path touches no allocator (sparse `Arg::Str` aside, which only appears on cold paths).
/// Carries **no address** (ADR-0031 step 7): internal wires route by connection, and the OSC
/// boundary stamps the node address from [`Plan::outbound_taps`](crate::plan::Plan), not from here.
#[derive(Debug, Clone)]
pub struct Emit {
    /// Which Message output port it went to, as an ordinal among the operator's Message
    /// outputs (a separate index space from Buffer/Signal outputs).
    pub port: usize,
    /// The single typed payload.
    pub arg: Arg,
    /// Sample offset within the Render block. Segment-relative when the operator calls `emit`;
    /// the engine stamps it block-absolute.
    pub frame: usize,
}

/// A routed event handed to an operator for one (sub)block. A zero-copy view onto the
/// originating block [`Message`]: the address *local* to the receiving node, a borrowed
/// reference to the one [`Arg`], and a segment-relative frame. The Render loop builds these in
/// place (no allocation), keeping Render realtime-safe while delivering events.
///
/// This is the raw delivered form; the typed read API (`io.input::<Note>`) decodes the borrowed
/// `Arg` into the operator's requested payload type. Carries **no address** (ADR-0031 step 7): a
/// delivered event is identified by the input port it lands on (the wired connection), not a name.
#[derive(Debug, Clone, Copy)]
pub struct Event<'a> {
    /// The single typed payload, borrowed from the source Message.
    pub arg: &'a Arg,
    /// Sample offset within the current (sub)block at which this event applies.
    pub frame: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vocab::{FilterMode, GateMode};

    /// `Arg` stays small (ADR-0030). Collapsing eight 1-byte vocab-enum variants into one
    /// `Enum(u32)` must not grow it: its size is dominated by the `Harmony` struct, and the bare
    /// index fits in the discriminant padding. Guards against a regression that would re-bloat the
    /// hot-path carrier (latched per input port, `Copy`-cloned each routed message).
    #[test]
    fn arg_stays_small() {
        assert!(
            std::mem::size_of::<Arg>() <= std::mem::size_of::<Harmony>(),
            "Arg ({}B) must not exceed its largest payload Harmony ({}B)",
            std::mem::size_of::<Arg>(),
            std::mem::size_of::<Harmony>(),
        );
    }

    /// A vocab enum round-trips through the type-erased `Arg::Enum(index)`: `From` packs the
    /// variant index, the typed read (`FromArg`) recovers the concrete variant. Two distinct enums
    /// share the one variant — identity is the reader's (`from_arg::<T>`), per port-authority.
    #[test]
    fn enum_round_trips_through_index() {
        let a: Arg = FilterMode::Bp.into();
        assert!(matches!(a, Arg::Enum(_)));
        assert_eq!(FilterMode::from_arg(&a), Some(FilterMode::Bp));

        // Same erased form, different reader type → that type's variant at the same index.
        let g: Arg = GateMode::DEFAULT.into();
        assert_eq!(GateMode::from_arg(&g), Some(GateMode::DEFAULT));

        // A numeric primitive is not an enum; the typed read declines.
        assert_eq!(FilterMode::from_arg(&Arg::F32(1.0)), None);
    }
}

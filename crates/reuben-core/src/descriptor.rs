//! Descriptor — an Operator's self-description.
//!
//! Separate from the process function, the descriptor lists ports and rich param
//! metadata. It is the seat of "good button" (auto-generated controls that can't sound
//! bad), of serialization, of connection type-checking, and of AI grounding — the `describe`
//! projections are derived from these descriptors.

// The scalar-control metadata types are owned by `reuben-contract` (issue #217): one
// `F32Meta`/`I32Meta`/`Curve` definition shared by the contract spec, the macro, and this
// runtime descriptor. Re-exported here so the macro-emitted path
// `::reuben_core::descriptor::F32Meta` and every in-crate `descriptor::` consumer keep working.
pub use reuben_contract::{Curve, F32Meta, I32Meta};

/// What a port carries — **the port's [`Arg`](crate::message::Arg) type**. Replaces
/// the retired `Shape`: delivery and read-style are no longer a declared axis, they follow from
/// the Arg type plus the handle's form (`io.read` on an `Event<Note>` / `Held<T>` handle). One variant per `Arg` *family*; a
/// vocab type names itself by its Arg variant (`Vocab { name: "Note", .. }` ↔ `Arg::Note`), which
/// keeps this enum from re-enumerating every vocab type as the central `Arg` already does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortType {
    /// A scalar number — a held (ZOH) control: freq, cutoff, amp. The port's [`F32Meta`] gives
    /// its good-button range / curve / unwired default. An `F32`-source wired into a [`Buffer`]
    /// port ZOH-materializes (the one implicit bridge).
    ///
    /// [`Buffer`]: PortType::F32Buffer
    F32,
    /// A discrete integer control / constant. `meta` is `Some` for a bounded settable
    /// integer (a count like the voicer's `voices` pool size), carrying its range + default in
    /// [`I32Meta`]; `None` for a bare integer atom with no declared range.
    I32 { meta: Option<I32Meta> },
    /// A string / symbol atom — cold / boundary paths only. Its `Arg` is `Arc<str>`-backed
    /// (issue #206), so forwarding one across the render thread is a refcount bump;
    /// construction still allocates and stays on the cold paths.
    Str,
    /// A dense per-sample signal (audio): the **only** Arg with a buffer form. A `Buffer`-source
    /// wired into a scalar port is illegal — it needs an explicit sampler op. Not
    /// boundary-crossable (no OSC form), which is how audio is kept off the wire by construction.
    F32Buffer,
    /// A shared *vocab* concrete type, named by its [`Arg`](crate::message::Arg) variant
    /// (`"Note"`, `"Harmony"`, `"SnapTarget"`). `enum_meta` is `Some` for a vocab **enum** — its
    /// variants + default + resolver, single-sourced from the type's `#[derive(ArgValue)]`
    /// (`T::enum_meta(name)`) so the descriptor and the type cannot drift — and `None` for a
    /// struct vocab type (`Note`, `Harmony`).
    ///
    /// `is_event` makes [event-ness](crate::plan::PortKind::Event) explicit on the type rather than
    /// inferred from `name`: `true` for an unlatched stream (`Note`), `false` for a latched
    /// [`Value`](crate::plan::PortKind::Value) (`Harmony`, and every enum). Carrying the flag here
    /// keeps [`port_kind`](crate::plan::port_kind) data-driven, so a second held struct vocab is not
    /// silently classified as an Event by a name check.
    Vocab {
        name: &'static str,
        is_event: bool,
        enum_meta: Option<EnumMeta>,
    },
    /// A **type-agnostic pass-through** (issue #141): the port carries *any*
    /// [`Arg`](crate::message::Arg), committing to no vocab type. Classified as an
    /// [Event](crate::plan::PortKind::Event) stream, so routing delivers the raw `Arg` unlatched
    /// and uncoerced; the operator reads and re-emits it through `Raw` handles (`io.read` on an
    /// `In<Raw>`, `io.write` on an `Out<Raw>`). The form the `osc_out` boundary sink's input takes, so any
    /// Message-domain value (a scalar echo, a vocab enum, a `Note`) can reach the wire and the
    /// type-driven expansion happens at the boundary ([`osc_out_args`](crate::boundary::osc_out_args)).
    /// **Input-only** (the contract validator fails an `arg` output/constant closed), and legal
    /// only where the operator treats the payload as opaque — a pure carrier: the wired *source*
    /// port is the type authority. Legality is capability-keyed
    /// ([`has_osc_form`](crate::boundary::has_osc_form)): any Event or Value source whose type
    /// has an external OSC form wires in — for a struct vocab type that means a converter
    /// registered via `register_osc_form!` ([`OscForm`](crate::boundary::OscForm), epic #146);
    /// a no-form source (`Harmony`, which registers none) is rejected at load/plan, and a
    /// Signal (audio) source likewise — audio stays off the wire by construction.
    Arg,
}

/// The scalar **element** a buffer port's dense per-sample block carries. Today the only buffer
/// form ([`F32Buffer`](PortType::F32Buffer)) is `f32`-element, so this has a single variant; it
/// exists so a site can ask *what a buffer carries* by name instead of spelling the variant, and so
/// a second buffer element type (issue #560) adds a variant here rather than another ad-hoc match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementType {
    /// A 32-bit float sample — the element of every buffer today.
    F32,
}

impl PortType {
    /// Whether this port carries a **dense per-sample buffer** (a Signal carrier) rather than a
    /// latched atom — the structural "is this a buffer?" question, true for the buffer variant(s)
    /// (today only [`F32Buffer`](PortType::F32Buffer)). Prefer this over
    /// `matches!(ty, PortType::F32Buffer)` at classification sites so buffer-ness reads as a
    /// question, not a variant spelling (issue #560). Defined as "has an element type", so the two
    /// predicates cannot drift.
    pub fn is_buffer(&self) -> bool {
        self.element_ty().is_some()
    }

    /// The [`ElementType`] a buffer port's samples carry, or `None` for a non-buffer (latched) port.
    /// Element-erased classification ([`PortKind::Signal`](crate::plan::PortKind::Signal)) is a
    /// *form*, not a type; this is the type descriptor for the sites that need the element, and the
    /// seam a second buffer element type plugs into (issue #560).
    pub fn element_ty(&self) -> Option<ElementType> {
        match self {
            PortType::F32Buffer => Some(ElementType::F32),
            _ => None,
        }
    }
}

impl core::fmt::Display for PortType {
    /// The short author-facing type name load errors print (`F32`, `F32Buffer`, `Note`,
    /// `Waveform`, …): a vocab port names its concrete type, everything else its `Arg` family.
    /// The `Debug` form is unfit for errors — a vocab enum's `Debug` dumps its whole [`EnumMeta`].
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PortType::F32 => write!(f, "F32"),
            PortType::I32 { .. } => write!(f, "I32"),
            PortType::Str => write!(f, "Str"),
            PortType::F32Buffer => write!(f, "F32Buffer"),
            PortType::Vocab { name, .. } => write!(f, "{name}"),
            PortType::Arg => write!(f, "Arg"),
        }
    }
}

/// Metadata for a vocab **enum** port: the closed, ordered set of named choices an
/// author may pick, the unwired default, and a type-erased resolver — all single-sourced from the
/// type's `#[derive(ArgValue)]` via `T::enum_meta(name)`, so the descriptor and the type cannot
/// drift.
///
/// `variants` are the stable wire **symbols** — the type's `VARIANTS` (PascalCase Rust idents,
/// `"Up"`, `"Sine"`). A variant's position is its on-wire integer **index** (the fallback form).
/// See [`resolve`](Self::resolve) (cold, string) and [`resolve_arg`](Self::resolve_arg) (hot,
/// alloc-free) for the symbol-primary / index-fallback binding.
#[derive(Debug, Clone)]
pub struct EnumMeta {
    /// The **port** name this metadata is attached to (`"dir"`).
    pub name: &'static str,
    /// The vocab **type** name — its `Arg` variant (`"SnapDir"`). The [`PortType::Vocab`]
    /// dispatch key for the boundary; distinct from `name` (the port).
    pub type_name: &'static str,
    pub variants: &'static [&'static str],
    /// Index into `variants` of the unwired default choice.
    pub default: usize,
    /// Type-erased resolver (derive-generated): normalize any [`Arg`](crate::message::Arg) form —
    /// the concrete variant, a [`Str`](crate::message::Arg::Str) symbol, or an
    /// [`I32`](crate::message::Arg::I32)/[`F32`](crate::message::Arg::F32) index — to this enum's
    /// concrete `Arg` variant (the `Copy`-normalized latch value), or `None`. Alloc-free; the
    /// render/latch and boundary path. The descriptor holds it as a `fn` pointer so routing can
    /// resolve an enum control message without knowing the concrete `T`.
    pub resolve: fn(&crate::message::Arg) -> Option<crate::message::Arg>,
}

// `resolve` is a fn pointer — comparing it is meaningless (clippy
// `unpredictable_function_pointer_comparisons`) and redundant: it is derive-generated from the
// type, so equal `type_name`s already imply equal resolvers. Compare the data fields only.
impl PartialEq for EnumMeta {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.type_name == other.type_name
            && self.variants == other.variants
            && self.default == other.default
    }
}
impl Eq for EnumMeta {}

impl EnumMeta {
    /// Resolve a wire **token** to a variant index — the cold, string form of the Enum-over-OSC
    /// binding. A **symbol** (`"Up"`) is matched against [`variants`](Self::variants); that is the
    /// primary, human-legible form an author writes (`"dir": "Up"`) and an OSC string carries. A
    /// bare **integer** (`"1"`) is accepted as a fallback index, in range. `None` if it is neither
    /// a known symbol nor an in-range index. Used by the loader / schema (no audio thread).
    pub fn resolve(&self, token: &str) -> Option<usize> {
        if let Some(i) = self.variants.iter().position(|v| *v == token) {
            return Some(i);
        }
        token
            .parse::<usize>()
            .ok()
            .filter(|&i| i < self.variants.len())
    }

    /// Resolve an [`Arg`](crate::message::Arg) to this enum's concrete `Arg` variant without
    /// allocating — the render-thread form, delegating to the derive-generated
    /// [`resolve`](Self::resolve) fn pointer. Used by routing so an enum control message
    /// (`/snap/dir "Up"`) touches no allocator on the audio thread.
    pub fn resolve_arg(&self, arg: &crate::message::Arg) -> Option<crate::message::Arg> {
        (self.resolve)(arg)
    }

    /// The default variant's symbol.
    pub fn default_symbol(&self) -> &'static str {
        self.variants[self.default]
    }

    /// The wire **symbol** for a concrete enum [`Arg`](crate::message::Arg) — the inverse of
    /// [`resolve_arg`](Self::resolve_arg), for the save path. Matches by `Copy`-normalized
    /// equality against each variant; `None` if `arg` is not one of this enum's variants. Cold.
    pub fn symbol_of(&self, arg: &crate::message::Arg) -> Option<&'static str> {
        (0..self.variants.len())
            .find(|&i| {
                self.resolve_arg(&crate::message::Arg::I32(i as i32))
                    .as_ref()
                    == Some(arg)
            })
            .map(|i| self.variants[i])
    }
}

/// A named input or output port.
///
/// `ty` is the sole axis: the port's [`Arg`](crate::message::Arg) type says what it
/// carries; delivery and read-style follow from that plus the read verb. `meta` is `Some` only for
/// a scalar [`F32`](PortType::F32) control input that owns its unwired default and is materialized
/// from a latched scalar. A [`Buffer`](PortType::F32Buffer) audio input and vocab ports
/// leave `meta` `None`. A vocab **enum** carries its [`EnumMeta`] inside its
/// [`PortType::Vocab`] (reach it via [`enum_meta`](Self::enum_meta)).
#[derive(Debug, Clone)]
pub struct Port {
    pub name: &'static str,
    pub ty: PortType,
    pub meta: Option<F32Meta>,
}

impl Port {
    /// A dense per-sample signal port (audio) — [`PortType::F32Buffer`]. The audio-passthrough input
    /// (no owned default) and the per-sample output an operator fills via `io.write` on a Signal handle.
    /// Replaces the legacy bare `signal` carrier.
    pub const fn f32_buffer(name: &'static str) -> Self {
        Self {
            name,
            ty: PortType::F32Buffer,
            meta: None,
        }
    }

    /// A signal port that *also* carries a scalar default + knob range.
    /// Classifies [`Signal`](crate::plan::PortKind::Signal) — so a Signal source (LFO / envelope)
    /// wires straight in with no converter — yet when unwired or knob-set it still materializes a
    /// per-sample buffer ZOH from `meta.default`, exactly like [`f32`](Self::f32). The form a
    /// signal-modulatable control (`oscillator.freq`, `filter.cutoff`) takes so it can accept
    /// modulation without flipping to Value (where an LFO wire would be a hard S→V mismatch).
    pub fn f32_buffer_meta(name: &'static str, meta: F32Meta) -> Self {
        Self {
            name,
            ty: PortType::F32Buffer,
            meta: Some(meta),
        }
    }

    /// A type-agnostic pass-through port — [`PortType::Arg`] (issue #141): carries any
    /// [`Arg`](crate::message::Arg) as a raw Event stream. The `osc_out` sink's input form.
    pub const fn arg(name: &'static str) -> Self {
        Self {
            name,
            ty: PortType::Arg,
            meta: None,
        }
    }

    /// A struct vocab port — [`PortType::Vocab`] with no enum metadata. `type_name` is the type's
    /// `Arg` variant name (`"Note"`, `"Harmony"`). `is_event` marks an unlatched stream (`Note`)
    /// versus a latched Value (`Harmony`). The `note`/`harmony` helpers wrap this.
    pub const fn vocab(name: &'static str, type_name: &'static str, is_event: bool) -> Self {
        Self {
            name,
            ty: PortType::Vocab {
                name: type_name,
                is_event,
                enum_meta: None,
            },
            meta: None,
        }
    }

    /// A `Note`-event port (replaces the legacy `message` carrier).
    pub const fn note(name: &'static str) -> Self {
        Self::vocab(name, "Note", true)
    }

    /// A `Harmony` port (replaces the legacy `context` carrier).
    pub const fn harmony(name: &'static str) -> Self {
        Self::vocab(name, "Harmony", false)
    }

    /// A held `Pitch` leaf port — a latched Value, like `harmony`. The output an
    /// `unpack_<type>` operator emits for a `Pitch` field; `resolve` (#523) reads it.
    pub const fn pitch(name: &'static str) -> Self {
        Self::vocab(name, "Pitch", false)
    }

    /// A scalar [`F32`](PortType::F32) control input: one input declared once, carrying
    /// its own unwired default in `meta`. When unwired the engine ZOH-materializes a per-sample
    /// buffer from the latched default (writing mid-block changes at their frame); when wired into
    /// a buffer-consuming op the source materializes likewise. Replaces the legacy "signal port +
    /// a same-named param" pair with a single declaration.
    pub fn f32(name: &'static str, meta: F32Meta) -> Self {
        Self {
            name,
            ty: PortType::F32,
            meta: Some(meta),
        }
    }

    /// A bounded scalar **integer** port carrying its range + default in [`I32Meta`].
    /// Today the form a plan-time [`Constant`](Descriptor::constants) count takes (the voicer's
    /// `voices` pool size); a settable integer whose value rides the wire as [`Arg::I32`].
    /// Parallel to [`f32`](Self::f32): the port owns its name, the meta is nameless (#213).
    pub fn i32(name: &'static str, meta: I32Meta) -> Self {
        Self {
            name,
            ty: PortType::I32 { meta: Some(meta) },
            meta: None,
        }
    }

    /// A vocab **enum** input: a held, live-switchable named choice (snap `dir`, osc
    /// `waveform`). Build `meta` from the type via `T::enum_meta(name)` so it cannot drift. An
    /// enum change rides the message wire as a block-sliced discrete update.
    pub fn enumerated(meta: EnumMeta) -> Self {
        Self {
            name: meta.name,
            ty: PortType::Vocab {
                name: meta.type_name,
                // An enum is a latched Value, never an event stream.
                is_event: false,
                enum_meta: Some(meta),
            },
            meta: None,
        }
    }

    /// This port's [`EnumMeta`] if it is a vocab enum, else `None`.
    pub fn enum_meta(&self) -> Option<&EnumMeta> {
        match &self.ty {
            PortType::Vocab {
                enum_meta: Some(e), ..
            } => Some(e),
            _ => None,
        }
    }

    /// Whether this is a scalar [`F32`](PortType::F32) control input the engine materializes a
    /// latched buffer for when unwired, rather than handing the operator `None`.
    pub fn is_materialized(&self) -> bool {
        self.meta.is_some()
    }

    /// Coerce an author literal [`Arg`](crate::message::Arg) to this port's normalized latch value
    /// — the single type-aware seam every authoring path funnels through. A scalar
    /// [`F32`](PortType::F32) control clamps to its [`F32Meta`] range; a vocab **enum** resolves a
    /// symbol / index / concrete variant to its `Copy`-normalized `Arg`. `None` when this port takes
    /// no settable literal (a bare audio buffer, a `Note` stream) or the literal does not resolve.
    pub fn coerce(&self, raw: &crate::message::Arg) -> Option<crate::message::Arg> {
        use crate::message::Arg;
        match &self.ty {
            PortType::F32 | PortType::F32Buffer if self.meta.is_some() => {
                let v = raw.as_f32()?;
                Some(Arg::F32(self.meta.as_ref()?.clamp(v)))
            }
            PortType::I32 { meta: Some(m) } => {
                let v = raw.as_f32()?.round() as i32;
                Some(Arg::I32(m.clamp(v)))
            }
            PortType::Vocab {
                enum_meta: Some(e), ..
            } => e.resolve_arg(raw),
            _ => None,
        }
    }

    /// This port's declared **numeric** unwired default, whichever meta slot carries it: the
    /// [`F32Meta`] field for an `f32`/`f32_buffer` port, the [`I32Meta`] *inside*
    /// [`PortType::I32`] for an integer one. `None` for a port with no number default (a bare
    /// audio buffer, an enum, a `Note` stream).
    ///
    /// The two homes are a wart of the descriptor's shape, not a distinction a caller asking
    /// "what does this port default to" should have to know. Widened to `f64` so both answer in
    /// one type — every value either slot can hold is exact there.
    pub fn number_default(&self) -> Option<f64> {
        match &self.ty {
            PortType::I32 { meta } => meta.as_ref().map(|m| f64::from(m.default)),
            _ => self.meta.as_ref().map(|m| f64::from(m.default)),
        }
    }
}

/// A declared **resource slot**: external data (a sample) a node depends on, named so the
/// loader knows which nodes need a ref, the format can validate the node's `sample` field,
/// and the schema / AI-grounding can express it. Distinct from params (which are
/// `f32`) and ports (which carry edges) — a resource is decoded once and bound out-of-band
/// via [`Operator::bind_resources`](crate::operator::Operator::bind_resources).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceSlot {
    pub name: &'static str,
}

impl ResourceSlot {
    pub const fn new(name: &'static str) -> Self {
        Self { name }
    }
}

/// An Operator's full self-description.
#[derive(Debug, Clone)]
pub struct Descriptor {
    /// Stable type name, also the default address segment.
    pub type_name: &'static str,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
    /// Instantiate-time **`Constant`** ports: plan-time config that, if changed, rebuilds
    /// the graph (e.g. the voicer's `voices` pool size). Each is an *immutable* [`Port`] — same type +
    /// meta as a runtime input, but it carries no edge/buffer and the loader routes it to the patch's
    /// `config` block, never `inputs`. Empty for the common operator. Runtime vs plan-time is which
    /// list a port lives in: [`inputs`](Self::inputs) (runtime) or here (plan-time).
    pub constants: Vec<Port>,
    /// Declared resource slots — external data this operator binds out-of-band.
    /// Empty for every operator that is a pure function of inputs + edges (all but the
    /// sample player today).
    pub resources: Vec<ResourceSlot>,
}

impl Descriptor {
    /// Index of a [`Constant`](Self::constants) port by name. `None` if `name` is not a constant.
    pub fn constant_index(&self, name: &str) -> Option<usize> {
        self.constants.iter().position(|p| p.name == name)
    }

    /// The [`Constant`](Self::constants) port named `name` — instantiate-time config the
    /// loader routes to the patch's `config` block, not `inputs`. `None` if `name` is not a constant.
    pub fn constant(&self, name: &str) -> Option<&Port> {
        self.constants.iter().find(|p| p.name == name)
    }

    /// Whether `name` is one of this operator's [`Constant`](Self::constants) ports.
    pub fn is_constant(&self, name: &str) -> bool {
        self.constants.iter().any(|p| p.name == name)
    }

    /// Resolve a [`Constant`](Self::constants) by `name` and [`coerce`](Port::coerce) `raw` to its
    /// stored [`Arg`](crate::message::Arg) — the constant-side dispatch behind
    /// [`Graph::set_constant`](crate::graph::Graph::set_constant). `None` if `name` is not a constant
    /// or `raw` does not resolve to its type.
    pub fn coerce_constant(
        &self,
        name: &str,
        raw: &crate::message::Arg,
    ) -> Option<(usize, crate::message::Arg)> {
        self.constants
            .iter()
            .enumerate()
            .find(|(_, p)| p.name == name)
            .and_then(|(i, p)| p.coerce(raw).map(|a| (i, a)))
    }

    /// Whether this operator declares a resource slot of the given name.
    pub fn has_resource(&self, name: &str) -> bool {
        self.resources.iter().any(|r| r.name == name)
    }

    /// Index + metadata of a scalar [`F32`](PortType::F32) control input named `name`,
    /// for routing an incoming `/node/<name> v` message to its latch/materialize buffer instead of
    /// a param slot. `None` for buffer inputs (no `meta`) and non-inputs.
    pub fn materialized_input(&self, name: &str) -> Option<(usize, &F32Meta)> {
        self.inputs
            .iter()
            .enumerate()
            .find(|(_, p)| p.name == name && p.is_materialized())
            .and_then(|(i, p)| p.meta.as_ref().map(|m| (i, m)))
    }

    /// Whether a **numeric literal** in a document's `inputs` block can set the input named
    /// `name` — the load-time gate on `{ "cutoff": 3000 }`.
    ///
    /// Asked through [`Port::coerce`], the function that would actually convert the literal,
    /// rather than by restating which port types accept one. The restatement is what broke: the
    /// gate used to read [`materialized_input`](Self::materialized_input), which answers out of
    /// the [`F32Meta`] struct field, so every `i32` port — whose meta lives *inside*
    /// [`PortType::I32`] — was invisible and a literal on one was refused as an unknown input
    /// (issue #569). Deriving the predicate from the conversion keeps the two from drifting again
    /// the next time a number type lands.
    ///
    /// Probed with a canonical in-range value rather than the author's, because this answers
    /// *"is this input settable by a number"*, not *"is this particular number good"*. An
    /// out-of-range literal keeps its existing treatment downstream — clamped for a number,
    /// dropped for an enum index — instead of being relabelled an unknown input.
    pub fn accepts_number_literal(&self, name: &str) -> bool {
        self.inputs
            .iter()
            .any(|p| p.name == name && p.coerce(&crate::message::Arg::F32(0.0)).is_some())
    }

    /// Every input carrying an [`F32Meta`] — each scalar [`F32`](PortType::F32) control and each
    /// signal port with a scalar default — paired with that meta.
    ///
    /// **Not** the set an author may write a numeric literal on, despite the name: it answers out
    /// of the `F32Meta` slot alone, so it omits every [`I32`](PortType::I32) control (issue #569).
    /// Use [`accepts_number_literal`](Self::accepts_number_literal) for that question. Currently
    /// callerless.
    pub fn settable_inputs(&self) -> impl Iterator<Item = (&'static str, &F32Meta)> {
        self.inputs
            .iter()
            .filter_map(|p| p.meta.as_ref().map(|m| (p.name, m)))
    }

    /// Index + metadata of a vocab **enum** input named `name`, for resolving a
    /// `/node/<name> "Up"` symbol (or fallback index) to its held variant. `None` for non-enum
    /// inputs and non-inputs.
    pub fn enum_input(&self, name: &str) -> Option<(usize, &EnumMeta)> {
        self.inputs
            .iter()
            .enumerate()
            .find(|(_, p)| p.name == name && p.enum_meta().is_some())
            .and_then(|(i, p)| p.enum_meta().map(|m| (i, m)))
    }

    /// Resolve a settable input by `name` and [`coerce`](Port::coerce) `raw` to its latch
    /// [`Arg`](crate::message::Arg) — the input-side dispatch behind
    /// [`Graph::set_value`](crate::graph::Graph::set_value). `None` if `name` is not a settable input
    /// or `raw` does not resolve to that input's type.
    pub fn coerce_input(
        &self,
        name: &str,
        raw: &crate::message::Arg,
    ) -> Option<(usize, crate::message::Arg)> {
        self.inputs
            .iter()
            .enumerate()
            .find(|(_, p)| p.name == name)
            .and_then(|(i, p)| p.coerce(raw).map(|a| (i, a)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two buffer predicates cannot drift: `is_buffer` is exactly "has an element type", and
    /// only [`PortType::F32Buffer`] carries one today (`f32`). Every latched form is neither.
    #[test]
    fn is_buffer_and_element_ty_agree() {
        assert!(PortType::F32Buffer.is_buffer());
        assert_eq!(PortType::F32Buffer.element_ty(), Some(ElementType::F32));

        for ty in [
            PortType::F32,
            PortType::I32 { meta: None },
            PortType::Str,
            PortType::Arg,
        ] {
            assert!(!ty.is_buffer(), "{ty} must not classify as a buffer");
            assert_eq!(ty.element_ty(), None, "{ty} carries no buffer element");
        }
    }
}

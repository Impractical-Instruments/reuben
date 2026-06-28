//! Descriptor — an Operator's self-description (ADR-0004).
//!
//! Separate from the process function, the descriptor lists ports and rich param
//! metadata. It is the seat of "good button" (auto-generated controls that can't sound
//! bad), of serialization, of connection type-checking, and of AI grounding. Run 2
//! generates the JSON schema from these descriptors.

/// What a port carries — **the port's [`Arg`](crate::message::Arg) type** (ADR-0030). Replaces
/// the retired `Shape`: delivery and read-style are no longer a declared axis, they follow from
/// the Arg type plus the read verb (`io.stream` / `io.last`). One variant per `Arg` *family*; a
/// vocab type names itself by its Arg variant (`Vocab { name: "Note", .. }` ↔ `Arg::Note`), which
/// keeps this enum from re-enumerating every vocab type as the central `Arg` already does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortType {
    /// A scalar number — a held (ZOH) control: freq, cutoff, amp. The port's [`ParamMeta`] gives
    /// its good-button range / curve / unwired default. An `F32`-source wired into a [`Buffer`]
    /// port ZOH-materializes (ADR-0030, the one implicit bridge).
    ///
    /// [`Buffer`]: PortType::F32Buffer
    F32,
    /// A discrete integer.
    I32,
    /// A string / symbol atom — cold / boundary paths only.
    Str,
    /// A dense per-sample signal (audio): the **only** Arg with a buffer form. A `Buffer`-source
    /// wired into a scalar port is illegal — it needs an explicit sampler op (ADR-0030). Not
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
}

/// The shape of an instantiate-time [`Constant`](crate::descriptor::ConstantShape) — config that,
/// if changed, would rebuild the graph (e.g. `voices`). Not a runtime [`PortType`]; a runtime
/// integer is a rounded [`F32`](PortType::F32) or an enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstantShape {
    Int,
    Enum,
}

/// Metadata for a vocab **enum** port (ADR-0030): the closed, ordered set of named choices an
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
}

/// A named input or output port.
///
/// `ty` is the sole axis (ADR-0030): the port's [`Arg`](crate::message::Arg) type says what it
/// carries; delivery and read-style follow from that plus the read verb. `meta` is `Some` only for
/// a scalar [`F32`](PortType::F32) control input that owns its unwired default and is materialized
/// from a latched scalar (ADR-0030). A [`Buffer`](PortType::F32Buffer) audio input and vocab ports
/// leave `meta` `None`. A vocab **enum** carries its [`EnumMeta`] inside its
/// [`PortType::Vocab`] (reach it via [`enum_meta`](Self::enum_meta)).
#[derive(Debug, Clone)]
pub struct Port {
    pub name: &'static str,
    pub ty: PortType,
    pub meta: Option<ParamMeta>,
}

impl Port {
    /// A dense per-sample signal port (audio) — [`PortType::F32Buffer`]. The audio-passthrough input
    /// (no owned default) and the per-sample output an operator fills with `io.signal_mut`.
    /// Replaces the legacy bare `signal` carrier.
    pub const fn f32_buffer(name: &'static str) -> Self {
        Self {
            name,
            ty: PortType::F32Buffer,
            meta: None,
        }
    }

    /// A signal port that *also* carries a scalar default + knob range (ADR-0031, decision (a)).
    /// Classifies [`Signal`](crate::plan::PortKind::Signal) — so a Signal source (LFO / envelope)
    /// wires straight in with no converter — yet when unwired or knob-set it still materializes a
    /// per-sample buffer ZOH from `meta.default`, exactly like [`f32`](Self::f32). The form a
    /// signal-modulatable control (`oscillator.freq`, `filter.cutoff`) takes so it can accept
    /// modulation without flipping to Value (where an LFO wire would be a hard S→V mismatch).
    pub fn f32_buffer_meta(meta: ParamMeta) -> Self {
        Self {
            name: meta.name,
            ty: PortType::F32Buffer,
            meta: Some(meta),
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

    /// A scalar [`F32`](PortType::F32) control input (ADR-0030): one input declared once, carrying
    /// its own unwired default in `meta`. When unwired the engine ZOH-materializes a per-sample
    /// buffer from the latched default (writing mid-block changes at their frame); when wired into
    /// a buffer-consuming op the source materializes likewise. Replaces the legacy "signal port +
    /// a same-named param" pair with a single declaration.
    pub fn f32(meta: ParamMeta) -> Self {
        Self {
            name: meta.name,
            ty: PortType::F32,
            meta: Some(meta),
        }
    }

    /// A vocab **enum** input (ADR-0030): a held, live-switchable named choice (snap `dir`, osc
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
}

/// A declared **resource slot**: external data (a sample) a node depends on, named so the
/// loader knows which nodes need a ref, the format can validate the node's `sample` field,
/// and the schema / AI-grounding can express it (ADR-0016). Distinct from params (which are
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

/// How a control responds across its range — the good-button curve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Curve {
    Linear,
    /// Perceptually-even for frequency-like params.
    Exponential,
}

/// Rich metadata for one parameter — enough to render a good-button control and to
/// ground an AI author.
#[derive(Debug, Clone)]
pub struct ParamMeta {
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    /// Display unit, e.g. "Hz", "dB", "s".
    pub unit: &'static str,
    pub curve: Curve,
}

impl ParamMeta {
    pub fn clamp(&self, v: f32) -> f32 {
        v.clamp(self.min, self.max)
    }
}

/// An Operator's full self-description.
#[derive(Debug, Clone)]
pub struct Descriptor {
    /// Stable type name, also the default address segment.
    pub type_name: &'static str,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
    pub params: Vec<ParamMeta>,
    /// Declared resource slots (ADR-0016) — external data this operator binds out-of-band.
    /// Empty for every operator that is a pure function of params + edges (all but the
    /// sample player today).
    pub resources: Vec<ResourceSlot>,
    /// The slot of the one param that is an instantiate-time **`Constant`** (ADR-0028) — config
    /// that, if changed, rebuilds the graph (e.g. the voicer's `voices` pool size). Declared
    /// directly via the contract's `constant:` keyword. `None` for the common operator.
    pub constant_param: Option<usize>,
}

impl Descriptor {
    /// Index of a param by name, for routing param Messages to slots.
    pub fn param_index(&self, name: &str) -> Option<usize> {
        self.params.iter().position(|p| p.name == name)
    }

    /// The one param that is a **`Constant`** (ADR-0028): instantiate-time config that, if changed,
    /// would rebuild the graph (e.g. the voicer's `voices` pool size). Declared via the contract's
    /// `constant:` keyword. The loader routes it to the patch's `config` block, not `inputs`.
    pub fn constant_param(&self) -> Option<&ParamMeta> {
        self.constant_param.and_then(|slot| self.params.get(slot))
    }

    /// Whether `name` is this operator's [`Constant`](Self::constant_param) param.
    pub fn is_constant_param(&self, name: &str) -> bool {
        self.constant_param().is_some_and(|p| p.name == name)
    }

    /// Default values for every param, in slot order.
    pub fn default_params(&self) -> Vec<f32> {
        self.params.iter().map(|p| p.default).collect()
    }

    /// Whether this operator declares a resource slot of the given name (ADR-0016).
    pub fn has_resource(&self, name: &str) -> bool {
        self.resources.iter().any(|r| r.name == name)
    }

    /// Index + metadata of a scalar [`F32`](PortType::F32) control input named `name` (ADR-0030),
    /// for routing an incoming `/node/<name> v` message to its latch/materialize buffer instead of
    /// a param slot. `None` for buffer inputs (no `meta`) and non-inputs.
    pub fn materialized_input(&self, name: &str) -> Option<(usize, &ParamMeta)> {
        self.inputs
            .iter()
            .enumerate()
            .find(|(_, p)| p.name == name && p.is_materialized())
            .and_then(|(i, p)| p.meta.as_ref().map(|m| (i, m)))
    }

    /// Every input an author may set as a **numeric literal** (ADR-0030): each scalar
    /// [`F32`](PortType::F32) control input, paired with its [`ParamMeta`]. The JSON-schema generator and the
    /// CLI `describe` both surface these alongside the real params (the old "signal port +
    /// same-named unwired-default param" is now one input), so reading them from this single
    /// definition keeps the two from drifting. Enums are a separate, non-numeric settable surface.
    pub fn settable_inputs(&self) -> impl Iterator<Item = (&'static str, &ParamMeta)> {
        self.inputs
            .iter()
            .filter_map(|p| p.meta.as_ref().map(|m| (p.name, m)))
    }

    /// Every vocab **enum** input an author may set as a **named choice** (ADR-0030), paired with
    /// its [`EnumMeta`]. The non-numeric sibling of [`settable_inputs`](Self::settable_inputs): the
    /// JSON-schema generator and the CLI `describe` both surface these (variants + default) so an
    /// author can set e.g. snap `dir` to `"Up"`. Single definition keeps the two from drifting.
    pub fn enum_inputs(&self) -> impl Iterator<Item = (&'static str, &EnumMeta)> {
        self.inputs
            .iter()
            .filter_map(|p| p.enum_meta().map(|e| (p.name, e)))
    }

    /// Index + metadata of a vocab **enum** input named `name` (ADR-0030), for resolving a
    /// `/node/<name> "Up"` symbol (or fallback index) to its held variant. `None` for non-enum
    /// inputs and non-inputs.
    pub fn enum_input(&self, name: &str) -> Option<(usize, &EnumMeta)> {
        self.inputs
            .iter()
            .enumerate()
            .find(|(_, p)| p.name == name && p.enum_meta().is_some())
            .and_then(|(i, p)| p.enum_meta().map(|m| (i, m)))
    }
}

//! Descriptor — an Operator's self-description (ADR-0004).
//!
//! Separate from the process function, the descriptor lists ports and rich param
//! metadata. It is the seat of "good button" (auto-generated controls that can't sound
//! bad), of serialization, of connection type-checking, and of AI grounding. Run 2
//! generates the JSON schema from these descriptors.

/// What kind of data a port carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortKind {
    /// Audio-rate [`crate::signal::Block`].
    Signal,
    /// Discrete [`crate::message::Message`] stream.
    Message,
    /// Latched tonal [`crate::context::Context`] — a struct-valued read service over the
    /// Message wire (ADR-0015): a context node publishes it; followers read "the current
    /// value". Carries no Signal buffer; the value rides a dedicated context arena.
    Context,
}

/// The single axis describing an [`Input`](Port)/output (ADR-0028): one closed, named set
/// from which delivery and read-style **follow**. There is no separate temporality axis and
/// no author-visible carrier.
///
/// During the migration (ADR-0028 is landed phase-by-phase) [`PortKind`] still rides on every
/// [`Port`] as the legacy carrier; `shape` is the forward-looking view. The two map 1:1 today
/// (`Signal→Float`, `Message→Note`, `Context→Harmony`) and [`PortKind`] is retired once every
/// operator is migrated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// A number — freq, cutoff, amp, a contour, a control. Always materialized to a per-sample
    /// buffer (ADR-0028); read per-sample (`io.signal`) or block-rate (`io.value`).
    Float,
    /// A named discrete choice (filter `mode`, osc `waveform`). A held scalar, block-sliced.
    Enum,
    /// The tonal-context struct (`root`/`scale`/`chord` + resolvers). A held struct, block-sliced.
    Harmony,
    /// A pitch/velocity event. A sparse, frame-stamped event list.
    Note,
}

impl PortKind {
    /// The forward-looking [`Shape`] this legacy carrier maps onto (ADR-0028). Used while both
    /// coexist so unmigrated operators present a `shape` without re-declaring their ports.
    pub const fn shape(self) -> Shape {
        match self {
            PortKind::Signal => Shape::Float,
            PortKind::Message => Shape::Note,
            PortKind::Context => Shape::Harmony,
        }
    }
}

/// The shape of an instantiate-time [`Constant`](ConstantMeta) (ADR-0028) — config that, if
/// changed, would rebuild the graph (e.g. `voices`). Not a runtime [`Shape`]; a runtime integer
/// is a rounded `Float` or an `Enum`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstantShape {
    Int,
    Enum,
}

/// Metadata for an [`Shape::Enum`] input (ADR-0028): the closed, ordered set of named choices an
/// author may pick, plus which one is the unwired default.
///
/// `variants` are the stable wire **symbols** — PascalCase Rust identifiers (`"Lp"`, `"Sine"`) —
/// emitted by the `operator_contract!`-generated `Enum` type (its `VARIANTS`), so the descriptor
/// and the type never drift. A variant's position is its on-wire integer **index** (the fallback
/// form). See [`EnumMeta::resolve`] for the symbol-primary / index-fallback binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumMeta {
    pub name: &'static str,
    pub variants: &'static [&'static str],
    /// Index into `variants` of the unwired default choice.
    pub default: usize,
}

impl EnumMeta {
    /// Resolve a wire token to a variant index — the ADR-0028 **Enum-over-OSC binding**. A
    /// **symbol** (`"Hp"`) is matched against [`variants`](Self::variants); that is the primary,
    /// human-legible form an author writes (`"mode": "Hp"`) and an OSC string carries. A bare
    /// **integer** (`"1"`) is accepted as a fallback index, in range. `None` if it is neither a
    /// known symbol nor an in-range index.
    pub fn resolve(&self, token: &str) -> Option<usize> {
        if let Some(i) = self.variants.iter().position(|v| *v == token) {
            return Some(i);
        }
        token
            .parse::<usize>()
            .ok()
            .filter(|&i| i < self.variants.len())
    }

    /// The default variant's symbol.
    pub fn default_symbol(&self) -> &'static str {
        self.variants[self.default]
    }
}

/// A named input or output port.
///
/// `kind` is the legacy [`PortKind`] carrier (ADR-0017); `shape` is the ADR-0028 axis. `meta`
/// is `Some` only for a **new-style materialized [`Shape::Float`] input** — an input that owns
/// its unwired default and is served as a per-sample buffer the engine fills from a latched
/// scalar (ADR-0028 materialize). A legacy `signal` input leaves `meta` `None` and keeps the
/// old "unwired ⇒ `None`, fall back to a same-named param" behavior until its operator migrates.
/// `enum_meta` is `Some` only for an [`Shape::Enum`] input.
#[derive(Debug, Clone)]
pub struct Port {
    pub name: &'static str,
    pub kind: PortKind,
    pub shape: Shape,
    pub meta: Option<ParamMeta>,
    pub enum_meta: Option<EnumMeta>,
}

impl Port {
    pub const fn signal(name: &'static str) -> Self {
        Self {
            name,
            kind: PortKind::Signal,
            shape: Shape::Float,
            meta: None,
            enum_meta: None,
        }
    }
    pub const fn message(name: &'static str) -> Self {
        Self {
            name,
            kind: PortKind::Message,
            shape: Shape::Note,
            meta: None,
            enum_meta: None,
        }
    }
    pub const fn context(name: &'static str) -> Self {
        Self {
            name,
            kind: PortKind::Context,
            shape: Shape::Harmony,
            meta: None,
            enum_meta: None,
        }
    }

    /// A **new-style materialized [`Shape::Float`] input** (ADR-0028): one `Input` declared once,
    /// carrying its own unwired default in `meta`. When unwired the engine materializes a
    /// per-sample buffer from the latched default (and writes mid-block changes at their frame);
    /// when wired it passes the source buffer through. Replaces the legacy "signal port + a
    /// same-named param" pair with a single declaration.
    pub fn float(meta: ParamMeta) -> Self {
        Self {
            name: meta.name,
            kind: PortKind::Signal,
            shape: Shape::Float,
            meta: Some(meta),
            enum_meta: None,
        }
    }

    /// An [`Shape::Enum`] input (ADR-0028): a held, live-switchable named choice (filter `mode`,
    /// osc `waveform`). `kind` is set to the legacy [`PortKind::Message`] carrier — an honest
    /// placeholder (an `Enum` change rides the message wire as a block-sliced discrete update);
    /// `Enum` is new in ADR-0028 and has no true legacy carrier, so this field is vestigial here
    /// and retired with [`PortKind`] once the sweep completes. `shape`/`enum_meta` are the truth.
    pub fn enumerated(meta: EnumMeta) -> Self {
        Self {
            name: meta.name,
            kind: PortKind::Message,
            shape: Shape::Enum,
            meta: None,
            enum_meta: Some(meta),
        }
    }

    /// Whether this is a new-style materialized Float input (ADR-0028) — the engine fills a
    /// latched buffer for it when unwired, rather than handing the operator `None`.
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

/// How an operator sets the Lane (Voice) count of its outputs (ADR-0010).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LaneRule {
    /// Lane count = the max of the operator's input Lane counts (1 if it has none).
    /// The default: ordinary single-Lane operators are replicated to match their inputs.
    #[default]
    Inherit,
    /// This operator *expands* the Lane count: it produces as many Lanes as the value of
    /// the named param slot (rounded, min 1). The Voicer is the canonical expander —
    /// `voices` Lanes out, regardless of input. Read once at Instantiate (structural).
    FromParam(usize),
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
    /// How this operator determines its output Lane count. Defaults to [`LaneRule::Inherit`].
    pub lanes: LaneRule,
}

impl Descriptor {
    /// Index of a param by name, for routing param Messages to slots.
    pub fn param_index(&self, name: &str) -> Option<usize> {
        self.params.iter().position(|p| p.name == name)
    }

    /// Default values for every param, in slot order.
    pub fn default_params(&self) -> Vec<f32> {
        self.params.iter().map(|p| p.default).collect()
    }

    /// Whether this operator declares a resource slot of the given name (ADR-0016).
    pub fn has_resource(&self, name: &str) -> bool {
        self.resources.iter().any(|r| r.name == name)
    }

    /// Index + metadata of a **new-style materialized [`Shape::Float`] input** named `name`
    /// (ADR-0028), for routing an incoming `/node/<name> v` message to its latch/materialize
    /// buffer instead of a param slot. `None` for legacy signal inputs (no `meta`) and non-inputs.
    pub fn materialized_input(&self, name: &str) -> Option<(usize, &ParamMeta)> {
        self.inputs
            .iter()
            .enumerate()
            .find(|(_, p)| p.name == name && p.is_materialized())
            .and_then(|(i, p)| p.meta.as_ref().map(|m| (i, m)))
    }

    /// Index + metadata of an [`Shape::Enum`] input named `name` (ADR-0028), for resolving a
    /// `/node/<name> "Hp"` symbol (or fallback index) to its held variant. `None` for non-enum
    /// inputs and non-inputs.
    pub fn enum_input(&self, name: &str) -> Option<(usize, &EnumMeta)> {
        self.inputs
            .iter()
            .enumerate()
            .find(|(_, p)| p.name == name && p.enum_meta.is_some())
            .and_then(|(i, p)| p.enum_meta.as_ref().map(|m| (i, m)))
    }
}

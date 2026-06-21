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

/// A named input or output port.
#[derive(Debug, Clone)]
pub struct Port {
    pub name: &'static str,
    pub kind: PortKind,
}

impl Port {
    pub const fn signal(name: &'static str) -> Self {
        Self {
            name,
            kind: PortKind::Signal,
        }
    }
    pub const fn message(name: &'static str) -> Self {
        Self {
            name,
            kind: PortKind::Message,
        }
    }
    pub const fn context(name: &'static str) -> Self {
        Self {
            name,
            kind: PortKind::Context,
        }
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
}

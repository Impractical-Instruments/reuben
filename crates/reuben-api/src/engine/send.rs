//! The one step a `send` takes before its batch reaches a host's ingress: compiling every string
//! aimed at an input that compiles its text, against the Plan installed.
//!
//! The render thread never compiles. It takes a compiled value only at an input whose compiler
//! built it, and only while the Plan the value was compiled against is still the live one: a
//! [`PreparedAgainst`] names that Plan, and a host's control drain asks [`control_fate`] before
//! queueing a batch.

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use reuben_core::coordinator::RenderSlot;
use reuben_core::message::Arg;
use reuben_core::plan::PreparedRoutes;

use super::wire::{ControlArg, ControlMessage, SendRefusal};

/// The Plan a message's compiled value was compiled against.
///
/// It holds that Plan's routing table, which each Plan builds for itself, so a pointer compare
/// tells whether the live Plan is the same one. Holding it also keeps the table's allocation, so
/// a later Plan's table cannot reuse the address while a message still names this one.
#[derive(Clone)]
pub struct PreparedAgainst(Arc<PreparedRoutes>);

impl PreparedAgainst {
    /// The Plan whose routing table is `routes`.
    pub fn new(routes: Arc<PreparedRoutes>) -> Self {
        Self(routes)
    }

    /// Whether `slot`'s live Engine runs the Plan this names. RT-safe: one pointer compare.
    pub fn is_live(&self, slot: &RenderSlot) -> bool {
        Arc::ptr_eq(&self.0, slot.prepared_routes())
    }
}

/// The same Plan: the same routing table, by pointer.
impl PartialEq for PreparedAgainst {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for PreparedAgainst {}

impl core::fmt::Debug for PreparedAgainst {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("PreparedAgainst")
            .field(&Arc::as_ptr(&self.0))
            .finish()
    }
}

/// What a host's control drain does with one batch, decided before any of it is queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlFate {
    /// Queue every message at the live Engine.
    Queue,
    /// Leave the batch where it is and come back to it: a swap is installing, so the live Engine
    /// is about to retire and anything queued on it may never render. The Engine that installs
    /// takes it.
    Wait,
    /// Queue none of it: its strings were compiled for a Plan a swap has since replaced. The
    /// drain counts every message and hands every address back off the render thread, so the
    /// gesture is reported whole rather than split across the swap.
    Drop,
}

/// Decide [`ControlFate`] for a batch whose messages carry `stamps` (their `against`), at `slot`.
/// Every compiled message of one `send` names the same Plan, so the first stamp decides.
///
/// Call [`RenderSlot::latch_install`] once before the drain, and fill after it: the waiting
/// answer is then the one the fill acts on, so a batch this queues renders on the Engine it was
/// queued on.
///
/// A stamp naming a Plan that is not live normally drops the batch. But if a swap was committed
/// after the latch, the batch was compiled for the Plan that swap installs, so it waits for it.
/// RT-safe: at most two `Acquire` loads and one pointer compare.
pub fn control_fate<'a>(
    slot: &RenderSlot,
    stamps: impl IntoIterator<Item = Option<&'a PreparedAgainst>>,
) -> ControlFate {
    if slot.install_pending() {
        return ControlFate::Wait;
    }
    match stamps.into_iter().flatten().next() {
        Some(against) if !against.is_live(slot) => {
            if slot.install_committed() {
                ControlFate::Wait
            } else {
                ControlFate::Drop
            }
        }
        _ => ControlFate::Queue,
    }
}

/// One message of a compiled `send` batch, as [`EngineHost::deliver_control`] receives it.
///
/// [`EngineHost::deliver_control`]: super::EngineHost::deliver_control
#[derive(Debug, Clone)]
pub struct PreparedMessage {
    pub address: String,
    /// The flat atoms, a compiled string as an [`Arg::Prepared`].
    pub args: Vec<Arg>,
    /// `Some` exactly when `args` holds a compiled value: the Plan it was compiled against. A
    /// host's drain asks [`control_fate`] with these before queueing the batch.
    pub against: Option<PreparedAgainst>,
}

/// The most bytes of the sent text a [`SendRefusal`] echoes: a window around the span, so the
/// refusal stays far under the channel's line cap whatever the request carried.
pub const REFUSAL_TEXT_WINDOW: usize = 1024;

/// The most bytes of a preparer's message a [`SendRefusal`] carries.
const REFUSAL_ERROR_BYTES: usize = 512;

/// What a refusal says of a message at a `str` input that is not one string.
pub const NOT_ONE_STRING: &str = "a str input takes one string";

/// Compile the string of every message aimed at an address in `routes`, the installed Plan's
/// table, with that address's preparer.
///
/// A message at such an address must be exactly one string: anything else would be dropped
/// unapplied at the input, so it refuses the batch instead. Every other message passes through as
/// its flat atoms. The first message refused refuses the batch, so no part of it is applied. Cold:
/// a preparer may allocate.
pub fn prepare_send(
    routes: &Arc<PreparedRoutes>,
    messages: Vec<ControlMessage>,
) -> Result<Vec<PreparedMessage>, SendRefusal> {
    let mut out = Vec::with_capacity(messages.len());
    for (i, message) in messages.into_iter().enumerate() {
        let ControlMessage { address, args } = message;
        let Some(route) = routes.get(&address) else {
            out.push(PreparedMessage {
                address,
                args: args.into_iter().map(Into::into).collect(),
                against: None,
            });
            continue;
        };
        let [ControlArg::Str(text)] = args.as_slice() else {
            return Err(SendRefusal {
                message: i,
                address,
                text: String::new(),
                offset: 0,
                start: 0,
                end: 0,
                error: NOT_ONE_STRING.to_string(),
            });
        };
        let source: Arc<str> = text.as_str().into();
        match route.preparer.prepare(&source) {
            Ok(value) => out.push(PreparedMessage {
                address,
                args: alloc::vec![Arg::Prepared(value)],
                against: Some(PreparedAgainst(Arc::clone(routes))),
            }),
            Err(e) => return Err(refusal(i, address, text, e.span, &e.message)),
        }
    }
    Ok(out)
}

/// A refusal of `text` at `span`, the text trimmed to at most [`REFUSAL_TEXT_WINDOW`] bytes around
/// the span's start, on `char` boundaries, and the span made relative to what is kept; its end
/// stays where it was, past the window when the window cut it.
fn refusal(
    message: usize,
    address: String,
    text: &str,
    span: core::ops::Range<usize>,
    error: &str,
) -> SendRefusal {
    let floor = |mut at: usize| {
        at = at.min(text.len());
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        at
    };
    let start = floor(span.start);
    let end = floor(span.end.max(span.start));
    let from = floor(start.saturating_sub(REFUSAL_TEXT_WINDOW / 2));
    let to = floor(from.saturating_add(REFUSAL_TEXT_WINDOW));
    let error = &error[..{
        let mut at = error.len().min(REFUSAL_ERROR_BYTES);
        while !error.is_char_boundary(at) {
            at -= 1;
        }
        at
    }];
    SendRefusal {
        message,
        address,
        text: text[from..to].to_string(),
        offset: from,
        start: start - from,
        end: end - from,
        error: error.to_string(),
    }
}

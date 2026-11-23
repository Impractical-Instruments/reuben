//! The one step a `send` takes before its batch reaches a host's ingress: compiling every string
//! aimed at an input that compiles its text, against the Plan installed.
//!
//! The render thread never compiles. It takes a compiled value only at an input whose compiler
//! built it, and only while the Plan the value was compiled against is still the live one: a
//! [`PreparedAgainst`] names that Plan, and a host's control drain asks it before queueing.

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

/// Queue one inbound message at `slot`'s live Engine, as [`RenderSlot::queue_osc`] does, unless
/// `against` names a Plan that is no longer live; `false` when it was dropped for that. A host's
/// control drain calls it for every message, a UDP one with `against` `None`, and counts and
/// reports the drops. RT-safe: a pointer compare, then `queue_osc`.
pub fn queue_control(
    slot: &mut RenderSlot,
    address: &str,
    args: &[Arg],
    against: Option<&PreparedAgainst>,
) -> bool {
    if against.is_some_and(|against| !against.is_live(slot)) {
        return false;
    }
    slot.queue_osc(address, args);
    true
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
    /// host's drain queues the message only while [`PreparedAgainst::is_live`] holds, and
    /// otherwise drops it, counts it, and names its address back off the render thread.
    pub against: Option<PreparedAgainst>,
}

/// Compile every lone string in `messages` aimed at an address in `routes`, the installed Plan's
/// table, with that address's preparer.
///
/// Every other message passes through as its flat atoms: a number, several atoms, an address with
/// no route. The first string that does not compile refuses the batch, so no part of it is
/// applied. Cold: a preparer may allocate.
pub fn prepare_send(
    routes: &Arc<PreparedRoutes>,
    messages: Vec<ControlMessage>,
) -> Result<Vec<PreparedMessage>, SendRefusal> {
    let mut out = Vec::with_capacity(messages.len());
    for (i, message) in messages.into_iter().enumerate() {
        let ControlMessage { address, args } = message;
        let route = routes.get(&address);
        let prepared = match (route, args.as_slice()) {
            (Some(route), [ControlArg::Str(text)]) => {
                let source: Arc<str> = text.as_str().into();
                match route.preparer.prepare(&source) {
                    Ok(value) => Some(value),
                    Err(e) => {
                        return Err(SendRefusal {
                            message: i,
                            address,
                            text: text.clone(),
                            start: e.span.start,
                            end: e.span.end,
                            error: e.message.to_string(),
                        })
                    }
                }
            }
            _ => None,
        };
        out.push(match prepared {
            Some(value) => PreparedMessage {
                address,
                args: alloc::vec![Arg::Prepared(value)],
                against: Some(PreparedAgainst(Arc::clone(routes))),
            },
            None => PreparedMessage {
                address,
                args: args.into_iter().map(Into::into).collect(),
                against: None,
            },
        });
    }
    Ok(out)
}

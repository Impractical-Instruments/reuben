//! The install mailbox's payload types: what crosses the RT boundary, and the render side's half
//! of a fresh Coordinator.
//!
//! Both are built off-thread and consumed here. They live on this side of the seam because the RT
//! install slot ([`super::slot::RenderSlot`]) owns them by value — it holds a
//! [`RenderMailbox<InstallBundle>`] and, when a reclaim cannot be posted back, a stranded
//! `Box<InstallBundle>`. Nothing about either type is document-shaped: the document that produced
//! the Engine stays with the Coordinator that loaded it.
//!
//! see rules: execution-runtime

use crate::engine::Engine;

use super::mailbox::RenderMailbox;
use super::migration::MigrationTable;

/// What crosses the install mailbox: a complete [`Engine`] — the Plan's runtime
/// vessel, so the callback allocates nothing post-install — plus the precomputed
/// [`MigrationTable`] the render side transplants by. This is the payload type the RT install slot
/// drains and applies; the retiree posted back is the same type (its `migration` is
/// then irrelevant — a reclaimed Engine has nothing to migrate).
pub struct InstallBundle {
    /// The freshly built Engine to install at the next callback top.
    pub engine: Engine,
    /// The `(old index, new index)` survivor pairs to transplant into `engine` from the retiring
    /// Engine before it goes live.
    pub migration: MigrationTable,
}

/// The render side's half of a fresh Coordinator: the **initial** Engine (installed directly into
/// the callback, not through the mailbox) and the [`RenderMailbox`] the callback drains. The
/// production RT slot ([`super::slot::RenderSlot`]) owns these; the Coordinator's initial install
/// hands them out so the shell can wire its audio callback.
pub struct RenderSide {
    pub engine: Engine,
    pub mailbox: RenderMailbox<InstallBundle>,
}

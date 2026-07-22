//! Signal — the audio-rate data vocabulary, named once.
//!
//! A [`Block`] is one block of audio for a single edge. CV and audio are the same thing: there is
//! no separate control-rate signal type. Sub-audio-rate control travels as
//! [`crate::message::Message`].
//!
//! This module is the **single place** the engine's audio element type and its buffer forms are
//! named:
//! - [`AudioSample`] — the audio element (`f32` today);
//! - [`Block`] — one **owned** block of audio for a single edge (a `Vec<AudioSample>`);
//! - [`BlockView`] / [`BlockMut`] — the **borrowed views** of a [`Block`] handed to an operator
//!   during Render (`&[AudioSample]` / `&mut [AudioSample]`).
//!
//! `Block` is the owned pool entry the Plan allocates; `BlockView`/`BlockMut` are the borrowed
//! operator views of it, so operators read and write audio without allocating — that is why both
//! the owned and the borrowed forms exist.
//!
//! Naming the element once, here, and referring to it everywhere else through these transparent
//! aliases means a future change to the audio element (say `f32` → a fixed-point or `f64` sample)
//! is a single edit at this site, not a repo-wide grep-and-replace. The aliases are **zero-cost**:
//! [`AudioSample`] *is* `f32`, [`BlockView`] *is* `&[f32]`, [`BlockMut`] *is* `&mut [f32]`, so
//! adopting them changes no layout, no ABI, and no runtime behavior.
//!
//! **The point is the boundary, not the abstraction.** Forcing every `f32` buffer site to spell
//! out whether it is "permanently audio" (adopt an alias) or "incidental `f32`" (keep the
//! primitive) surfaces a partition the codebase otherwise leaves implicit. The CI guard
//! `scripts/check_sample_alias.py` enforces the rule: a raw `f32` slice or `Vec` may appear only
//! in this file or in an explicit, justified allowlist (the device layer, resource decode, and
//! operator DSP arithmetic — where the `f32` is a device-native frame or a number under the math,
//! not a logical audio buffer). Clippy's `disallowed-types` cannot express this — a primitive has
//! no type path to disallow — which is why the guard is a text linter.

/// The engine's audio element. One block of audio is a run of these.
pub type AudioSample = f32;

/// One block of audio samples, length == `block_size`.
///
/// The **owned** form, backed by a `Vec<AudioSample>`. The Plan owns the pool of blocks used as
/// edge buffers; operators receive borrowed [`BlockView`]/[`BlockMut`] (sub)slices during Render
/// and never allocate.
pub type Block = Vec<AudioSample>;

/// A borrowed, read-only view of a [`Block`] — the shared-reference render form
/// (`&[AudioSample]`).
pub type BlockView<'a> = &'a [AudioSample];

/// A borrowed, writable view of a [`Block`] — the exclusive-reference render form
/// (`&mut [AudioSample]`).
pub type BlockMut<'a> = &'a mut [AudioSample];

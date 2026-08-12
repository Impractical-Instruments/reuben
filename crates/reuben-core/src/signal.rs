//! Signal — the audio-rate data vocabulary, named once.
//!
//! A [`Block`] is one block of audio for a single edge. CV and audio are the same thing: there is
//! no separate control-rate signal type. Sub-audio-rate control travels as
//! [`crate::message::Message`].
//!
//! This module is the **single place** the engine's audio element type and its buffer forms are
//! named:
//! - [`AudioSample`] — the audio element (`f32` today);
//! - [`Block`] — one **owned** block of audio for a single edge (a `Vec<AudioSample>`), the pool
//!   entry the Plan allocates;
//! - [`BlockView`] / [`BlockMut`] — the **borrowed views** of a [`Block`] handed to an operator
//!   during Render (`&[AudioSample]` / `&mut [AudioSample]`), so it touches audio without
//!   allocating. That is why both the owned and the borrowed forms exist.
//!
//! Adopting an alias elsewhere is not optional: `scripts/check_sample_alias.py` fails the build on
//! a raw `f32` slice or `Vec` outside this file and its justified allowlist.
//!
//! see rules: composition-operators

use alloc::vec::Vec;

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

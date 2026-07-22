//! Signal — the audio-rate data that flows on edges.
//!
//! A [`Block`] is one block of audio for a single edge. CV and audio are the same thing:
//! there is no separate control-rate signal type. Sub-audio-rate control
//! travels as [`crate::message::Message`].

/// One block of audio samples, length == `block_size`.
///
/// Backed by a `Vec<Sample>` (the audio element named once in [`crate::sample`]). The Plan owns
/// the pool of blocks used as edge buffers; operators receive borrowed (sub)slices during Render
/// and never allocate.
pub type Block = Vec<crate::sample::Sample>;

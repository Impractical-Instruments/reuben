//! Signal — the audio-rate data that flows on edges.
//!
//! A [`Block`] is one block of audio for a single Lane (one Voice × one Channel).
//! CV and audio are the same thing (ADR-0001): there is no separate control-rate
//! signal type. Sub-audio-rate control travels as [`crate::message::Message`].

/// One block of audio samples for a single Lane, length == `block_size`.
///
/// Backed by a `Vec<f32>` for the MVP. The Plan owns the pool of blocks used as edge
/// buffers; operators receive borrowed (sub)slices during Render and never allocate.
pub type Block = Vec<f32>;

/// Allocate a zeroed block of the given length.
pub fn zeroed_block(len: usize) -> Block {
    vec![0.0; len]
}

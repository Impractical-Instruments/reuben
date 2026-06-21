//! Live audio out via cpal.
//!
//! Opens the default output device, builds an [`Engine`] matched to the device sample
//! rate, and renders inside the audio callback. Incoming Messages are pulled from an
//! [`std::sync::mpsc::Receiver`] (fed by the OSC/UDP thread) at the top of each callback.
//! Mono core output is fanned out to every device channel.
//!
//! The returned [`cpal::Stream`] must be kept alive for audio to keep playing.

use std::fmt;
use std::sync::mpsc::Receiver;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};
use reuben_core::message::Message;
use reuben_core::AudioConfig;

use crate::engine::Engine;

/// Things that can go wrong opening the audio stream.
#[derive(Debug)]
pub enum AudioError {
    /// No default output device.
    NoDevice,
    /// The device reported an unusable default config.
    Config(cpal::DefaultStreamConfigError),
    /// The device's default sample format isn't supported (MVP handles f32 only).
    UnsupportedFormat(SampleFormat),
    /// Building the stream failed.
    Build(cpal::BuildStreamError),
    /// Starting playback failed.
    Play(cpal::PlayStreamError),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::NoDevice => write!(f, "no default output device"),
            AudioError::Config(e) => write!(f, "default output config: {e}"),
            AudioError::UnsupportedFormat(fmt) => {
                write!(f, "unsupported sample format {fmt:?} (only f32 for now)")
            }
            AudioError::Build(e) => write!(f, "build output stream: {e}"),
            AudioError::Play(e) => write!(f, "play stream: {e}"),
        }
    }
}

impl std::error::Error for AudioError {}

/// Start live playback on the default output device.
///
/// `block_size` is the core render block size; `make_engine` builds the engine once the
/// device sample rate is known (so the Plan's tuning matches the hardware). Returns the
/// live [`Stream`] — keep it alive.
pub fn start<F>(
    rx: Receiver<Message>,
    block_size: usize,
    make_engine: F,
) -> Result<Stream, AudioError>
where
    F: FnOnce(AudioConfig) -> Engine,
{
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or(AudioError::NoDevice)?;
    let supported = device.default_output_config().map_err(AudioError::Config)?;

    let sample_format = supported.sample_format();
    if sample_format != SampleFormat::F32 {
        return Err(AudioError::UnsupportedFormat(sample_format));
    }

    let config: cpal::StreamConfig = supported.into();
    let channels = config.channels as usize;
    let sample_rate = config.sample_rate.0 as f32;

    let mut engine = make_engine(AudioConfig::new(sample_rate, block_size));
    // Scratch for one callback's worth of mono samples; grows to the largest callback.
    let mut mono: Vec<f32> = Vec::new();

    let stream = device
        .build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                while let Ok(m) = rx.try_recv() {
                    engine.queue(m);
                }
                let frames = data.len() / channels;
                if mono.len() < frames {
                    mono.resize(frames, 0.0);
                }
                engine.fill(&mut mono[..frames]);
                for (frame, chunk) in data.chunks_mut(channels).enumerate() {
                    for out in chunk.iter_mut() {
                        *out = mono[frame];
                    }
                }
            },
            |err| eprintln!("audio stream error: {err}"),
            None,
        )
        .map_err(AudioError::Build)?;

    stream.play().map_err(AudioError::Play)?;
    Ok(stream)
}

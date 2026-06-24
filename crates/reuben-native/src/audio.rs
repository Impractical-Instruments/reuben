//! Live audio out via cpal.
//!
//! Opens the default output device, builds an [`Engine`] matched to the device sample
//! rate, and renders inside the audio callback. Incoming Messages are pulled from an
//! [`std::sync::mpsc::Receiver`] (fed by the OSC/UDP thread) at the top of each callback.
//!
//! This module owns the **logical→device channel map** (ADR-0026): the engine renders the
//! instrument's *logical* master channels (left/right/…), and [`map_frame`] places them onto
//! whatever channel count the real device has — a straight copy when they match, a downmix
//! for a mono device, and zero-fill for a device with more channels than the instrument uses.
//! Core never learns the device's channel count.
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
    let logical = engine.channels();
    // Scratch for one callback's worth of interleaved logical samples; grows to the largest
    // callback (audio-thread allocation only while warming up, never in steady state).
    let mut buf: Vec<f32> = Vec::new();

    let stream = device
        .build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                while let Ok(m) = rx.try_recv() {
                    engine.queue(m);
                }
                let frames = data.len() / channels;
                if buf.len() < frames * logical {
                    buf.resize(frames * logical, 0.0);
                }
                engine.fill(&mut buf[..frames * logical]);
                for (frame, dst) in data.chunks_mut(channels).enumerate() {
                    let src = &buf[frame * logical..frame * logical + logical];
                    map_frame(src, dst);
                }
            },
            |err| eprintln!("audio stream error: {err}"),
            None,
        )
        .map_err(AudioError::Build)?;

    stream.play().map_err(AudioError::Play)?;
    Ok(stream)
}

/// Place one frame of `logical` master channels onto a `device`-channel frame (ADR-0026).
///
/// - **Equal counts** → straight copy (the common stereo→stereo and the historical
///   mono-as-two → stereo case).
/// - **Mono device** → downmix: the mean of the logical channels, so nothing is lost.
/// - **More device channels than logical** → copy what exists, zero the extras.
/// - **Fewer device channels (but >1) than logical** → copy the leading channels, drop the
///   rest (only reachable with >2 logical channels, which v1 doesn't produce by default).
fn map_frame(logical: &[f32], device: &mut [f32]) {
    if device.len() == logical.len() {
        device.copy_from_slice(logical);
    } else if device.len() == 1 {
        device[0] = logical.iter().sum::<f32>() / logical.len() as f32;
    } else {
        for (d, out) in device.iter_mut().enumerate() {
            *out = logical.get(d).copied().unwrap_or(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::map_frame;

    #[test]
    fn stereo_to_stereo_is_straight_copy() {
        let mut dev = [0.0f32; 2];
        map_frame(&[0.25, -0.5], &mut dev);
        assert_eq!(dev, [0.25, -0.5]);
    }

    #[test]
    fn stereo_to_mono_downmixes() {
        let mut dev = [0.0f32; 1];
        map_frame(&[0.2, 0.4], &mut dev);
        assert!(
            (dev[0] - 0.3).abs() < 1e-6,
            "expected mean 0.3, got {}",
            dev[0]
        );
    }

    #[test]
    fn broadcast_mono_to_mono_is_bit_identical() {
        // The historical mono path: both logical channels carry the same value, so a mono
        // device gets exactly that value back (mean of two equal floats is the float).
        let mut dev = [0.0f32; 1];
        map_frame(&[0.123_456_79, 0.123_456_79], &mut dev);
        assert_eq!(dev[0].to_bits(), 0.123_456_79_f32.to_bits());
    }

    #[test]
    fn extra_device_channels_are_zeroed() {
        let mut dev = [9.0f32; 4];
        map_frame(&[0.1, 0.2], &mut dev);
        assert_eq!(dev, [0.1, 0.2, 0.0, 0.0]);
    }
}

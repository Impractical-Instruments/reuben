//! Live audio out via cpal.
//!
//! Opens the default output device, builds an [`Engine`] matched to the device sample
//! rate, and renders inside the audio callback. Incoming decoded OSC ([`OscIn`]) is pulled from an
//! [`std::sync::mpsc::Receiver`] (fed by the OSC/UDP thread) at the top of each callback and typed
//! to a Message against the Plan (ADR-0030).
//!
//! This module owns the **logical→device channel map** (ADR-0026): the engine renders the
//! instrument's *logical* master channels (left/right/…), and [`map_frame`] places them onto
//! whatever channel count the real device has — a straight copy when they match, a downmix
//! for a mono device, and zero-fill for a device with more channels than the instrument uses.
//! Core never learns the device's channel count.
//!
//! It also measures the callback against its own real-time budget (ADR-0038 §9, P6/#183): a
//! render that takes longer than the audio time it produced is an output xrun, counted through
//! the shared [`crate::diagnostics::Diagnostics`] surface — the device still plays its own
//! underrun silence, reuben only observes and counts it (fixed policy, no recovery mode).
//!
//! The returned [`cpal::Stream`] must be kept alive for audio to keep playing.

use std::fmt;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};
use reuben_core::message::Message;
use reuben_core::AudioConfig;

use crate::diagnostics::Diagnostics;
use crate::engine::Engine;
use crate::osc::OscIn;

/// How often the periodic diagnostics logger wakes to check the counters (ADR-0038 §9). It only
/// emits a line when something changed, so a healthy run stays quiet at this cadence regardless.
const DIAGNOSTICS_LOG_INTERVAL: Duration = Duration::from_secs(5);

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
/// device sample rate is known (so the Plan's tuning matches the hardware). `osc_out` is the
/// optional OSC-out sink (ADR-0026): when `Some`, the callback forwards each outbound Message to
/// it (a sender thread encodes + UDP-sends, off the audio thread); when `None`, outbound is
/// drained and dropped, with one warning the first time a rig actually sends. Returns the live
/// [`Stream`] (keep it alive) and the [`Diagnostics`] counters this callback feeds — hand the
/// same `Arc` to P5's input stream so both sides of the boundary share one counter surface
/// (ADR-0038 §9). A background thread is already logging it periodically; no further wiring is
/// required to get stderr output.
pub fn start<F>(
    rx: Receiver<OscIn>,
    block_size: usize,
    osc_out: Option<Sender<Message>>,
    make_engine: F,
) -> Result<(Stream, Arc<Diagnostics>), AudioError>
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
    // Warn at most once if a rig sends OSC out with no target configured (ADR-0026).
    let mut warned_no_target = false;

    let diagnostics = Diagnostics::new();
    let diag_for_callback = Arc::clone(&diagnostics);
    crate::diagnostics::spawn_periodic_logger(Arc::clone(&diagnostics), DIAGNOSTICS_LOG_INTERVAL);

    let stream = device
        .build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                // Real-time deadline measurement (ADR-0038 §9): `Instant::now()` reads the OS
                // monotonic clock, which on every platform reuben targets is a vDSO-mapped read
                // with no kernel trap, lock, or allocation — the same cost class as reading a
                // hardware counter, not a blocking syscall. That makes two reads per callback an
                // acceptable, deliberate exception to "no syscalls in the callback": it is the
                // pragmatic way to measure wall-clock render time from inside the render path
                // itself, and every reuben target platform (Linux/macOS/Windows) backs it this
                // way.
                let callback_start = Instant::now();

                while let Ok(m) = rx.try_recv() {
                    // Convert flat OSC -> typed Message at the engine, where the Plan (and so each
                    // dest port's Arg type) is known (ADR-0030).
                    engine.queue_osc(&m);
                }
                let frames = data.len() / channels;
                if buf.len() < frames * logical {
                    buf.resize(frames * logical, 0.0);
                }
                engine.fill(&mut buf[..frames * logical]);
                // Forward this callback's outbound Messages (ADR-0026). The sender thread does the
                // UDP I/O, so the audio thread only hands off. No target -> drain and drop, warning
                // once so a misconfigured feedback rig isn't silently dead.
                for m in engine.drain_outbound() {
                    match &osc_out {
                        Some(tx) => {
                            let _ = tx.send(m);
                        }
                        None => {
                            if !warned_no_target {
                                warned_no_target = true;
                                eprintln!(
                                    "warning: instrument sends OSC out but no --osc-out target; dropping"
                                );
                            }
                        }
                    }
                }
                for (frame, dst) in data.chunks_mut(channels).enumerate() {
                    let src = &buf[frame * logical..frame * logical + logical];
                    map_frame(src, dst);
                }

                // The budget is this callback's own frame count over the sample rate, not a
                // fixed `block_size / sample_rate`: cpal is free to ask for a different number of
                // frames than the core's render block (engine.rs already documents this — "a
                // cpal callback asks for an arbitrary number of frames at unpredictable times").
                // Using the actual `frames` generalizes the ADR's formula to that reality instead
                // of miscounting every callback whose size doesn't match `block_size`.
                let budget = callback_budget(frames, sample_rate);
                if callback_start.elapsed() > budget {
                    diag_for_callback.record_output_xrun();
                }
            },
            |err| eprintln!("audio stream error: {err}"),
            None,
        )
        .map_err(AudioError::Build)?;

    stream.play().map_err(AudioError::Play)?;
    Ok((stream, diagnostics))
}

/// The real-time budget for a callback rendering `frames` frames at `sample_rate`: how much
/// audio time this callback must produce within to keep up with the device (ADR-0038 §9's
/// `block_size / sample_rate`, generalized to the callback's actual frame count — see
/// [`start`]'s doc comment on why `frames` rather than the fixed core `block_size`).
fn callback_budget(frames: usize, sample_rate: f32) -> Duration {
    Duration::from_secs_f32(frames as f32 / sample_rate)
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
    use super::{callback_budget, map_frame};
    use std::time::Duration;

    #[test]
    fn callback_budget_matches_block_size_over_sample_rate() {
        // The ADR-0038 §9 formula: 256 frames at 48 kHz owes ~5.333 ms.
        let budget = callback_budget(256, 48_000.0);
        let expected = Duration::from_secs_f32(256.0 / 48_000.0);
        assert_eq!(budget, expected);
        assert!((budget.as_secs_f64() - 0.005_333).abs() < 1e-4);
    }

    #[test]
    fn callback_budget_scales_with_frame_count() {
        // cpal is free to ask for a different frame count callback to callback (engine.rs); the
        // budget must track whatever `frames` this particular callback actually rendered.
        // (Tolerance, not exact equality: `f32` division then `Duration` rounding means doubling
        // the frame count doesn't land on a bit-identical doubled `Duration`.)
        let one_block = callback_budget(256, 48_000.0);
        let two_blocks = callback_budget(512, 48_000.0);
        let diff = two_blocks.as_secs_f64() - 2.0 * one_block.as_secs_f64();
        assert!(
            diff.abs() < 1e-6,
            "expected ~double, got {two_blocks:?} vs 2x {one_block:?}"
        );
    }

    #[test]
    fn callback_budget_scales_with_sample_rate() {
        // The same frame count owes less wall-clock time at a higher sample rate.
        let at_48k = callback_budget(256, 48_000.0);
        let at_96k = callback_budget(256, 96_000.0);
        assert!(at_96k < at_48k);
    }

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

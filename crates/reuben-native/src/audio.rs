//! Live audio out via cpal.
//!
//! Opens an output device (the host default, or a [`DeviceProfile`]'s `output.device`
//! substring selection, ADR-0038 §6), builds an [`Engine`] matched to the device sample rate,
//! and renders inside the audio callback. Incoming decoded OSC ([`OscIn`]) is pulled from an
//! [`std::sync::mpsc::Receiver`] (fed by the OSC/UDP thread) at the top of each callback and typed
//! to a Message against the Plan (ADR-0030).
//!
//! This module owns the **logical→device channel map** (ADR-0026): the engine renders the
//! instrument's *logical* master channels (left/right/…), and [`map_frame`] places them onto
//! whatever channel count the real device has — a straight copy when they match, a downmix
//! for a mono device, and zero-fill for a device with more channels than the instrument uses.
//! An explicit `output.map` in the profile **overrides** that implicit policy entirely
//! ([`OutputMap::Explicit`], ADR-0038 §6/§7); no profile (or an empty map) keeps [`map_frame`]'s
//! behavior, bit-identical to before. Core never learns the device's channel count.
//!
//! `sample_rate`/`buffer_size` in the profile are **preferences**: [`negotiate_output_config`]
//! requests them against the device's supported configs and adopts whatever is granted,
//! logging the outcome (ADR-0038 §6/§8) — reuben never fights the device.
//!
//! It also measures the callback against its own real-time budget (ADR-0038 §9, P6/#183): a
//! render that takes longer than the audio time it produced is an output xrun, counted through
//! the shared [`crate::diagnostics::Diagnostics`] surface — the device still plays its own
//! underrun silence, reuben only observes and counts it (fixed policy, no recovery mode).
//!
//! When the played patch binds input channels (ADR-0038 §3), [`start`] also opens the input
//! side (P5/#182, [`crate::input`]): a cpal input stream feeding a lock-free SPSC ring that
//! this module's output callback drains — resampled and drift-compensated into the engine
//! rate — into [`Engine::fill_duplex`]. A patch without input pipes never touches an input
//! device.
//!
//! The returned [`Streams`] must be kept alive for audio to keep playing.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, SupportedBufferSize};
use reuben_core::message::Message;
use reuben_core::AudioConfig;

use crate::diagnostics::Diagnostics;
use crate::engine::Engine;
use crate::osc::OscIn;
use crate::profile::DeviceProfile;

/// How often the periodic diagnostics logger wakes to check the counters (ADR-0038 §9). It only
/// emits a line when something changed, so a healthy run stays quiet at this cadence regardless.
const DIAGNOSTICS_LOG_INTERVAL: Duration = Duration::from_secs(5);

/// Things that can go wrong opening the audio stream.
#[derive(Debug)]
pub enum AudioError {
    /// No default output device.
    NoDevice,
    /// No output device's name contains the profile's `output.device` substring.
    NoMatchingDevice(String),
    /// Enumerating output devices failed.
    DevicesQuery(cpal::DevicesError),
    /// The device reported an unusable default config.
    Config(cpal::DefaultStreamConfigError),
    /// Querying the device's supported configs failed (only reached when a profile requests a
    /// specific sample rate).
    SupportedConfigs(cpal::SupportedStreamConfigsError),
    /// The device's default sample format isn't supported (MVP handles f32 only).
    UnsupportedFormat(SampleFormat),
    /// Building the stream failed.
    Build(cpal::BuildStreamError),
    /// Starting playback failed.
    Play(cpal::PlayStreamError),
    /// The played patch binds input channels but there is no default input device (P5/#182).
    /// Fatal by design: the patch explicitly asked for live input, so playing silently
    /// without a device would violate ADR-0038 §9's "know and say" (§7's dark-degrade covers
    /// *channel* mismatches on a device that did open, not the absence of any device).
    NoInputDevice,
    /// No input device's name contains the profile's `input.device` substring.
    NoMatchingInputDevice(String),
    /// Enumerating input devices failed.
    InputDevicesQuery(cpal::DevicesError),
    /// The input device reported an unusable default config.
    InputConfig(cpal::DefaultStreamConfigError),
    /// The input device's default sample format isn't supported (f32 only, like output).
    UnsupportedInputFormat(SampleFormat),
    /// Building the input stream failed.
    BuildInput(cpal::BuildStreamError),
    /// Starting input capture failed.
    PlayInput(cpal::PlayStreamError),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::NoDevice => write!(f, "no default output device"),
            AudioError::NoMatchingDevice(s) => {
                write!(f, "no output device name contains {s:?}")
            }
            AudioError::DevicesQuery(e) => write!(f, "query output devices: {e}"),
            AudioError::Config(e) => write!(f, "default output config: {e}"),
            AudioError::SupportedConfigs(e) => {
                write!(f, "query supported output configs: {e}")
            }
            AudioError::UnsupportedFormat(fmt) => {
                write!(f, "unsupported sample format {fmt:?} (only f32 for now)")
            }
            AudioError::Build(e) => write!(f, "build output stream: {e}"),
            AudioError::Play(e) => write!(f, "play stream: {e}"),
            AudioError::NoInputDevice => write!(
                f,
                "instrument binds input channels but there is no default input device"
            ),
            AudioError::NoMatchingInputDevice(s) => {
                write!(f, "no input device name contains {s:?}")
            }
            AudioError::InputDevicesQuery(e) => write!(f, "query input devices: {e}"),
            AudioError::InputConfig(e) => write!(f, "default input config: {e}"),
            AudioError::UnsupportedInputFormat(fmt) => {
                write!(
                    f,
                    "unsupported input sample format {fmt:?} (only f32 for now)"
                )
            }
            AudioError::BuildInput(e) => write!(f, "build input stream: {e}"),
            AudioError::PlayInput(e) => write!(f, "play input stream: {e}"),
        }
    }
}

impl std::error::Error for AudioError {}

/// The live cpal streams [`start`] returns — keep the whole struct alive for audio to keep
/// flowing. `input` is `Some` only when the played patch binds input channels (ADR-0038 §3):
/// a patch without input pipes never touches an input device (P5/#182).
pub struct Streams {
    pub output: Stream,
    pub input: Option<Stream>,
}

/// Start live playback on an output device, per `profile` (ADR-0038 §6).
///
/// `block_size` is the core render block size; `make_engine` builds the engine once the
/// device sample rate is known (so the Plan's tuning matches the hardware). `osc_out` is the
/// optional OSC-out sink (ADR-0026): when `Some`, the callback forwards each outbound Message to
/// it (a sender thread encodes + UDP-sends, off the audio thread); when `None`, outbound is
/// drained and dropped, with one warning the first time a rig actually sends. `profile` selects
/// the devices, negotiates sample-rate/buffer-size preferences, and overrides the channel
/// maps — pass [`DeviceProfile::default`] for today's behavior (default devices, identity maps).
///
/// When the built engine binds input channels, the input side opens too (P5/#182,
/// [`crate::input`]): a cpal input stream on the profile's `input.device` (default input
/// device otherwise) feeds a lock-free ring; each output callback pulls that ring through the
/// resampling/drift-compensating [`crate::input::InputStage`] and hands the result to
/// [`Engine::fill_duplex`]. Dual-device and mismatched-rate setups work from day one
/// (ADR-0038 §8 — no same-device-only path).
///
/// Returns the live [`Streams`] (keep them alive) and the [`Diagnostics`] counters both
/// callbacks feed — one shared surface for output xruns and input-ring under/overruns
/// (ADR-0038 §9). A background thread is already logging it periodically; no further wiring
/// is required to get stderr output.
pub fn start<F>(
    rx: Receiver<OscIn>,
    block_size: usize,
    osc_out: Option<Sender<Message>>,
    profile: &DeviceProfile,
    make_engine: F,
) -> Result<(Streams, Arc<Diagnostics>), AudioError>
where
    F: FnOnce(AudioConfig) -> Engine,
{
    let host = cpal::default_host();
    let device = select_output_device(&host, profile.output.device.as_deref())?;

    let (sample_format, config) =
        negotiate_output_config(&device, profile.sample_rate, profile.buffer_size)?;
    if sample_format != SampleFormat::F32 {
        return Err(AudioError::UnsupportedFormat(sample_format));
    }

    let channels = config.channels as usize;
    let sample_rate = config.sample_rate.0 as f32;

    let mut engine = make_engine(AudioConfig::new(sample_rate, block_size));
    let logical = engine.channels();
    let in_channels = engine.input_channels();
    let output_map = build_output_map(&profile.output.map, logical, channels);
    // Scratch for one callback's worth of interleaved logical samples; grows to the largest
    // callback (audio-thread allocation only while warming up, never in steady state).
    let mut buf: Vec<f32> = Vec::new();
    // The input-side dual of `buf`: one callback's worth of interleaved logical *input* at
    // the engine rate, filled from the ring and consumed by `fill_duplex`. Same warmup-only
    // growth policy. Stays empty for a patch with no input pipes.
    let mut in_buf: Vec<f32> = Vec::new();
    // Warn at most once if a rig sends OSC out with no target configured (ADR-0026).
    let mut warned_no_target = false;

    let diagnostics = Diagnostics::new();
    let diag_for_callback = Arc::clone(&diagnostics);
    crate::diagnostics::spawn_periodic_logger(Arc::clone(&diagnostics), DIAGNOSTICS_LOG_INTERVAL);

    // Input opens ONLY when the played patch binds input channels (ADR-0038 §3/P5): a patch
    // without input pipes never touches an input device. The stage moves into the output
    // callback; the stream is played *after* the output side is running, so the ring prefill
    // starts with a live consumer.
    let (input_stream, mut input_stage) = if in_channels > 0 {
        let (stream, stage) = crate::input::open_input(
            &host,
            profile,
            in_channels,
            sample_rate,
            block_size,
            Arc::clone(&diagnostics),
        )?;
        (Some(stream), Some(stage))
    } else {
        (None, None)
    };

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
                match &mut input_stage {
                    Some(stage) => {
                        // Pull this callback's logical input (engine-rate, resampled +
                        // drift-compensated) off the ring, then render duplex (P5/#182).
                        if in_buf.len() < frames * in_channels {
                            in_buf.resize(frames * in_channels, 0.0);
                        }
                        stage.fill(&mut in_buf[..frames * in_channels]);
                        engine
                            .fill_duplex(&in_buf[..frames * in_channels], &mut buf[..frames * logical]);
                    }
                    None => engine.fill(&mut buf[..frames * logical]),
                }
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
                    apply_output_map(&output_map, src, dst);
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
    // Output first, then input: the output callback is already pulling (reading warmup
    // zeros), so the ring prefills against a live consumer instead of flooding.
    if let Some(input) = &input_stream {
        input.play().map_err(AudioError::PlayInput)?;
    }
    Ok((
        Streams {
            output: stream,
            input: input_stream,
        },
        diagnostics,
    ))
}

/// The real-time budget for a callback rendering `frames` frames at `sample_rate`: how much
/// audio time this callback must produce within to keep up with the device (ADR-0038 §9's
/// `block_size / sample_rate`, generalized to the callback's actual frame count — see
/// [`start`]'s doc comment on why `frames` rather than the fixed core `block_size`).
fn callback_budget(frames: usize, sample_rate: f32) -> Duration {
    let secs = frames as f32 / sample_rate;
    // `from_secs_f32` panics on non-finite/negative input; a device reporting a zero (or
    // garbage) sample rate must not become a panic inside the audio callback. No budget is
    // computable then, so nothing counts as a miss.
    if !secs.is_finite() || secs < 0.0 {
        return Duration::MAX;
    }
    Duration::from_secs_f32(secs)
}

/// Select an output device (ADR-0038 §6): `None` is the host default (today's only behavior);
/// `Some(substr)` is the first device whose name contains `substr`, case-insensitively.
fn select_output_device(
    host: &cpal::Host,
    name_substr: Option<&str>,
) -> Result<cpal::Device, AudioError> {
    match name_substr {
        None => host.default_output_device().ok_or(AudioError::NoDevice),
        Some(substr) => {
            let needle = substr.to_lowercase();
            host.output_devices()
                .map_err(AudioError::DevicesQuery)?
                .find(|d| {
                    d.name()
                        .map(|n| device_name_matches(&n, &needle))
                        .unwrap_or(false)
                })
                .ok_or_else(|| AudioError::NoMatchingDevice(substr.to_string()))
        }
    }
}

/// The case-insensitive substring match behind `output.device` selection (and, via
/// [`crate::input`], `input.device` selection), pulled out of [`select_output_device`] so it
/// has a unit test that doesn't need a real [`cpal::Host`] (review finding #6) — `needle` is
/// already lowercased by the caller (once per call, not per device).
pub(crate) fn device_name_matches(name: &str, needle_lower: &str) -> bool {
    name.to_lowercase().contains(needle_lower)
}

/// The outcome of matching a requested output sample rate against a device's supported configs
/// (review finding #2): a rate match is only "granted" at the device *default's* channel
/// count — a config that matches the rate but not the channel count would otherwise silently
/// hand back a different channel count than the caller (and `build_output_map`'s validation)
/// expect.
enum RateNegotiation {
    /// A config at the requested rate, at the device default's channel count.
    Granted(cpal::SupportedStreamConfig),
    /// No same-channel-count config matched the rate; this is the best rate match found, at a
    /// *different* channel count than the device default. Never returned silently — the caller
    /// must log it.
    ChannelCountChanged(cpal::SupportedStreamConfig),
    /// Nothing at all matched the requested rate.
    Unsupported,
}

/// Pure selection logic for [`negotiate_output_config`]'s sample-rate branch: no device I/O, so
/// it has a unit test that doesn't need a real [`cpal::Device`] (review finding #6). Prefers an
/// F32 config at `want` Hz whose channel count matches `default_channels`; only falls back to a
/// different channel count if nothing at `want` Hz matches it.
fn negotiate_rate(
    configs: &[cpal::SupportedStreamConfigRange],
    default_channels: cpal::ChannelCount,
    want: u32,
) -> RateNegotiation {
    let at_rate = || {
        configs.iter().filter(|r| {
            r.sample_format() == SampleFormat::F32
                && r.min_sample_rate().0 <= want
                && want <= r.max_sample_rate().0
        })
    };
    if let Some(r) = at_rate().find(|r| r.channels() == default_channels) {
        return RateNegotiation::Granted(r.with_sample_rate(cpal::SampleRate(want)));
    }
    match at_rate().next() {
        Some(r) => RateNegotiation::ChannelCountChanged(r.with_sample_rate(cpal::SampleRate(want))),
        None => RateNegotiation::Unsupported,
    }
}

/// Request → grant → adopt `sample_rate`/`buffer_size` preferences against `device`'s supported
/// configs (ADR-0038 §6/§8): reuben never fights the device, it logs what it asked for and what
/// it got. Neither preference set is bit-identical to before — the device's own default config,
/// untouched. A requested rate/size the device can't grant is a reality mismatch (§7): warn and
/// fall back/clamp, never fatal.
fn negotiate_output_config(
    device: &cpal::Device,
    sample_rate: Option<u32>,
    buffer_size: Option<u32>,
) -> Result<(SampleFormat, cpal::StreamConfig), AudioError> {
    let default_config = device.default_output_config().map_err(AudioError::Config)?;
    let supported = match sample_rate {
        None => default_config,
        Some(want) => {
            let default_channels = default_config.channels();
            let configs: Vec<_> = device
                .supported_output_configs()
                .map_err(AudioError::SupportedConfigs)?
                .collect();
            match negotiate_rate(&configs, default_channels, want) {
                RateNegotiation::Granted(cfg) => {
                    println!(
                        "io-map: requested output sample rate {want} Hz, device grants it \
                         ({default_channels} channel(s))"
                    );
                    cfg
                }
                RateNegotiation::ChannelCountChanged(cfg) => {
                    eprintln!(
                        "warning: io-map requested output sample rate {want} Hz at the device's \
                         default channel count ({default_channels}); no config matches both, \
                         granting {} channel(s) instead",
                        cfg.channels()
                    );
                    cfg
                }
                RateNegotiation::Unsupported => {
                    eprintln!(
                        "warning: io-map requested output sample rate {want} Hz; device doesn't \
                         support it, using its default {} Hz",
                        default_config.sample_rate().0
                    );
                    default_config
                }
            }
        }
    };

    let sample_format = supported.sample_format();
    let mut config: cpal::StreamConfig = supported.clone().into();

    if let Some(want) = buffer_size {
        config.buffer_size = match supported.buffer_size() {
            SupportedBufferSize::Range { min, max } => {
                let granted = want.clamp(*min, *max);
                if granted == want {
                    println!("io-map: requested output buffer size {want}, device grants it");
                } else {
                    eprintln!(
                        "warning: io-map requested output buffer size {want}; device supports \
                         {min}..={max}, using {granted}"
                    );
                }
                cpal::BufferSize::Fixed(granted)
            }
            SupportedBufferSize::Unknown => {
                println!(
                    "io-map: requested output buffer size {want}; device doesn't report a \
                     supported range, requesting it as-is"
                );
                cpal::BufferSize::Fixed(want)
            }
        };
    }

    Ok((sample_format, config))
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

/// The active output channel mapping (ADR-0038 §6/§7): [`OutputMap::Identity`] defers to
/// [`map_frame`]'s implicit broadcast/downmix/zero-fill policy; [`OutputMap::Explicit`] is a
/// profile's validated `output.map`, which **overrides** that policy entirely. Validated once,
/// at stream setup ([`build_output_map`]) — never re-checked per frame, since the logical and
/// device channel counts are both fixed once the stream is open.
enum OutputMap {
    Identity,
    Explicit {
        /// Validated `(logical, device)` pairs — both indices already checked in range.
        pairs: Vec<(usize, usize)>,
        /// `true` at index `d` for every device channel a pair targets, precomputed once here
        /// so [`apply_output_map`] can zero *only* the unmapped channels instead of zeroing the
        /// whole frame and then overwriting the mapped ones every callback (review finding #5).
        mapped: Vec<bool>,
    },
}

/// Build the active output map from a profile's `output.map` (ADR-0038 §6). An empty map (no
/// profile, or `output.map` omitted) is [`OutputMap::Identity`] — [`map_frame`]'s behavior,
/// unchanged. Otherwise every pair is checked against the real `logical`/`device` channel
/// counts once, here: a pair naming a channel that doesn't exist on either side is a reality
/// mismatch (ADR-0038 §7) — warned about now and dropped, not fatal. Two *different* logical
/// channels naming the *same* device channel are also a reality mismatch (review finding #1):
/// both pairs are kept (so the mapping is still fully described), but colliding targets are
/// warned about once, here, since [`apply_output_map`] applies pairs in ascending-logical order
/// and the higher logical channel silently wins otherwise.
fn build_output_map(map: &BTreeMap<usize, usize>, logical: usize, device: usize) -> OutputMap {
    if map.is_empty() {
        return OutputMap::Identity;
    }
    let mut pairs = Vec::with_capacity(map.len());
    for (&l, &d) in map {
        if l >= logical {
            eprintln!(
                "warning: io-map output.map logical channel {l} does not exist (instrument has \
                 {logical} logical channel(s)); dropped"
            );
            continue;
        }
        if d >= device {
            eprintln!(
                "warning: io-map output.map targets device channel {d}, but the device has \
                 {device} channel(s); dropped"
            );
            continue;
        }
        pairs.push((l, d));
    }
    warn_duplicate_device_targets(&pairs);
    let mut mapped = vec![false; device];
    for &(_, d) in &pairs {
        mapped[d] = true;
    }
    OutputMap::Explicit { pairs, mapped }
}

/// Warn about `output.map` pairs that target the same device channel from different logical
/// channels (review finding #1). `pairs` is in ascending-logical order (the `BTreeMap`'s
/// iteration order), and [`apply_output_map`] applies pairs in that same order, so — for a
/// colliding device channel — the *highest* logical channel in the collision is the one whose
/// value survives; named explicitly here so the behavior isn't just an implementation accident.
fn warn_duplicate_device_targets(pairs: &[(usize, usize)]) {
    let mut by_device: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for &(l, d) in pairs {
        by_device.entry(d).or_default().push(l);
    }
    for (d, logicals) in by_device {
        if logicals.len() > 1 {
            let winner = *logicals.last().expect("just checked len() > 1 above");
            eprintln!(
                "warning: io-map output.map targets device channel {d} from multiple logical \
                 channels {logicals:?}; logical channel {winner} wins (applied last), the rest \
                 are dropped for that device channel"
            );
        }
    }
}

/// Apply the active output mapping to one frame. `Identity` defers to [`map_frame`]'s policy;
/// `Explicit` zeros every device channel the map doesn't target (ADR-0038 §7's degrade-to-silence)
/// and then copies each validated `(logical, device)` pair. Allocation-free: `pairs`/`mapped` are
/// built once at stream setup, never in the render callback.
fn apply_output_map(map: &OutputMap, logical_frame: &[f32], device_frame: &mut [f32]) {
    match map {
        OutputMap::Identity => map_frame(logical_frame, device_frame),
        OutputMap::Explicit { pairs, mapped } => {
            for (d, out) in device_frame.iter_mut().enumerate() {
                if !mapped[d] {
                    *out = 0.0;
                }
            }
            for &(l, d) in pairs {
                device_frame[d] = logical_frame[l];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::time::Duration;

    #[test]
    fn callback_budget_survives_zero_sample_rate() {
        // A garbage device rate must not panic in the callback; an incomputable budget
        // means nothing ever counts as a miss.
        assert_eq!(callback_budget(256, 0.0), Duration::MAX);
        assert_eq!(callback_budget(256, -48_000.0), Duration::MAX);
    }

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

    #[test]
    fn empty_map_is_identity() {
        // No profile (or `output.map` omitted) builds `Identity` — ADR-0038 §6's bit-identical
        // no-profile guarantee starts here, before a frame is ever touched.
        let map = build_output_map(&BTreeMap::new(), 2, 2);
        assert!(matches!(map, OutputMap::Identity));
    }

    #[test]
    fn no_profile_output_is_bit_identical_to_map_frame() {
        // The load-bearing assertion (ADR-0038 §6/issue #181): with no profile, `apply_output_map`
        // must render exactly what `map_frame` renders today, sample-for-sample, for every shape
        // existing instruments hit (stereo, mono downmix, extra device channels).
        let cases: &[(&[f32], usize)] = &[
            (&[0.25, -0.5], 2),
            (&[0.2, 0.4], 1),
            (&[0.123_456_79, 0.123_456_79], 1),
            (&[0.1, 0.2], 4),
        ];
        let identity = build_output_map(&BTreeMap::new(), 2, 2); // channel counts unused by Identity
        for &(logical, device_channels) in cases {
            let mut want = vec![0.0f32; device_channels];
            map_frame(logical, &mut want);
            let mut got = vec![0.0f32; device_channels];
            apply_output_map(&identity, logical, &mut got);
            assert_eq!(
                want.iter().map(|f| f.to_bits()).collect::<Vec<_>>(),
                got.iter().map(|f| f.to_bits()).collect::<Vec<_>>(),
                "no-profile output must be bit-identical to map_frame for {logical:?} -> {device_channels} device channel(s)"
            );
        }
    }

    #[test]
    fn explicit_map_overrides_and_zero_fills_unmapped_targets() {
        let mut profile_map = BTreeMap::new();
        profile_map.insert(0, 2); // logical 0 -> device channel 2
        profile_map.insert(1, 0); // logical 1 -> device channel 0
        let map = build_output_map(&profile_map, 2, 4);
        let mut dev = [9.0f32; 4];
        apply_output_map(&map, &[0.5, -0.25], &mut dev);
        // device 0 <- logical 1 (-0.25), device 2 <- logical 0 (0.5), 1 and 3 unmapped -> zero.
        assert_eq!(dev, [-0.25, 0.0, 0.5, 0.0]);
    }

    #[test]
    fn explicit_map_drops_out_of_range_logical_channel() {
        let mut profile_map = BTreeMap::new();
        profile_map.insert(5, 0); // instrument only has 2 logical channels
        let map = build_output_map(&profile_map, 2, 2);
        match map {
            OutputMap::Explicit { pairs, .. } => {
                assert!(pairs.is_empty(), "out-of-range pair kept")
            }
            OutputMap::Identity => panic!("non-empty map must build Explicit"),
        }
    }

    #[test]
    fn explicit_map_drops_out_of_range_device_channel() {
        let mut profile_map = BTreeMap::new();
        profile_map.insert(0, 9); // device only has 2 channels
        let map = build_output_map(&profile_map, 2, 2);
        match map {
            OutputMap::Explicit { pairs, .. } => {
                assert!(pairs.is_empty(), "out-of-range pair kept")
            }
            OutputMap::Identity => panic!("non-empty map must build Explicit"),
        }
    }

    #[test]
    fn duplicate_device_targets_keep_both_pairs_deterministically() {
        // Review finding #1: two logical channels mapping to the same device channel is a
        // reality mismatch, not a silent last-write-wins. Both pairs are kept (so nothing is
        // dropped without a reason), and application order (ascending logical) determines which
        // one's value survives on that device channel.
        let mut profile_map = BTreeMap::new();
        profile_map.insert(0, 0);
        profile_map.insert(1, 0); // collides with logical 0 on device channel 0
        let map = build_output_map(&profile_map, 2, 2);
        match &map {
            OutputMap::Explicit { pairs, .. } => assert_eq!(pairs, &vec![(0, 0), (1, 0)]),
            OutputMap::Identity => panic!("non-empty map must build Explicit"),
        }
        let mut dev = [9.0f32; 2];
        apply_output_map(&map, &[0.1, 0.2], &mut dev);
        // Logical 1 is applied after logical 0 (ascending order), so it wins device channel 0.
        assert_eq!(dev, [0.2, 0.0]);
    }

    #[test]
    fn explicit_map_zeros_unmapped_channels_without_double_writing_mapped_ones() {
        // Review finding #5: mapped channels should be written exactly once per callback.
        let mut profile_map = BTreeMap::new();
        profile_map.insert(0, 1); // device channel 0 is left unmapped
        let map = build_output_map(&profile_map, 1, 2);
        match &map {
            OutputMap::Explicit { mapped, .. } => assert_eq!(mapped, &vec![false, true]),
            OutputMap::Identity => panic!("non-empty map must build Explicit"),
        }
        let mut dev = [9.0f32; 2];
        apply_output_map(&map, &[0.5], &mut dev);
        assert_eq!(dev, [0.0, 0.5]);
    }

    fn config(
        channels: cpal::ChannelCount,
        min: u32,
        max: u32,
    ) -> cpal::SupportedStreamConfigRange {
        cpal::SupportedStreamConfigRange::new(
            channels,
            cpal::SampleRate(min),
            cpal::SampleRate(max),
            SupportedBufferSize::Range { min: 64, max: 4096 },
            SampleFormat::F32,
        )
    }

    #[test]
    fn negotiate_rate_prefers_default_channel_count() {
        let configs = vec![config(1, 44_100, 48_000), config(2, 44_100, 48_000)];
        match negotiate_rate(&configs, 2, 48_000) {
            RateNegotiation::Granted(cfg) => {
                assert_eq!(cfg.channels(), 2);
                assert_eq!(cfg.sample_rate().0, 48_000);
            }
            _ => panic!("expected a same-channel-count grant"),
        }
    }

    #[test]
    fn negotiate_rate_falls_back_to_different_channel_count_and_says_so() {
        // Only a mono config supports the requested rate; the device default is stereo.
        let configs = vec![config(1, 88_200, 96_000)];
        match negotiate_rate(&configs, 2, 96_000) {
            RateNegotiation::ChannelCountChanged(cfg) => {
                assert_eq!(cfg.channels(), 1);
                assert_eq!(cfg.sample_rate().0, 96_000);
            }
            _ => panic!("expected a channel-count-changed grant"),
        }
    }

    #[test]
    fn negotiate_rate_unsupported_when_nothing_matches() {
        let configs = vec![config(2, 44_100, 48_000)];
        assert!(matches!(
            negotiate_rate(&configs, 2, 96_000),
            RateNegotiation::Unsupported
        ));
    }

    #[test]
    fn device_name_match_is_case_insensitive_substring() {
        assert!(device_name_matches("Scarlett 2i2 USB", "scarlett"));
        assert!(!device_name_matches("Built-in Output", "scarlett"));
    }
}

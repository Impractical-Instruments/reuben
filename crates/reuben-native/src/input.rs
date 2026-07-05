//! Live audio input via cpal (ADR-0038 §8/§9, P5/#182): the cross-thread, cross-clock path
//! from a real input device into the engine's logical input master (P3).
//!
//! ## Shape
//!
//! The engine stays hosted in the **output** callback, rendering at the output device's rate
//! (ADR-0038 §8 — the clock anchor). The input device runs its own callback on its own clock;
//! the two meet at a lock-free SPSC ring (`rtrb` — the primitive ADR-0002 anticipated):
//!
//! - **Producer** (the input callback): maps each device frame onto the instrument's *logical*
//!   input channels ([`InputMap`], the dual of `audio.rs`'s output `map_frame` — identity by
//!   default, a profile's `input.map` overrides; a logical channel the device/map can't supply
//!   reads silence with a one-time startup warning, ADR-0038 §7), then commits it to the ring
//!   **whole frames at a time** so the consumer never observes a torn frame.
//! - **Consumer** ([`InputStage`], owned by the output callback): pops device-rate frames and
//!   resamples them to the engine rate with a drift-servoed ratio, filling the interleaved
//!   logical input block that [`crate::engine::Engine::fill_duplex`] consumes.
//!
//! Both callbacks are RT-safe after startup: no allocation, no locks, no blocking — the ring
//! is preallocated, the resampler's state is two frames, and every policy decision is
//! arithmetic on values already in cache.
//!
//! ## Resampler choice (recorded per ADR-0038 §8)
//!
//! **Linear interpolation** over a two-frame window ([`LinearResampler`]). Rationale: it is
//! trivially RT-safe (no FIR history, no allocation, no library), it is *bit-exact* in the
//! dominant case (equal rates → ratio 1.0 → straight passthrough while the servo is
//! centered), and at the tiny ratio deviations drift compensation produces (|1 − r| ≤ 0.5%)
//! its passband error sits far below the noise floor of any live mic path. The ADR explicitly
//! allows modest starting quality; a windowed-sinc upgrade can replace [`LinearResampler`]
//! without touching the ring, the servo, or the policies. For genuinely mismatched rates
//! (44.1k mic into a 48k engine) linear interpolation images above ~17 kHz — audible on
//! bright synthetic material, acceptable for voice/instrument capture, and the recorded
//! starting point.
//!
//! ## Drift compensation
//!
//! Two devices are two clocks (§8's USB-mic argument): even at the same nominal rate they
//! drift, so any fixed ratio eventually starves or floods the ring. The [`DriftServo`]
//! measures the ring's **residual** fill after each output callback drains it and steers the
//! resample ratio to hold that residual at a fixed floor ([`RING_FLOOR_BLOCKS`] core blocks).
//! Servoing the *residual* (not the pre-consumption fill) makes the loop independent of the
//! callback size, which cpal varies freely. The correction is clamped to
//! ±[`DriftServo::MAX_CORRECTION`] (0.5% ≈ 8.6 cents, inaudible on live input) and one-pole
//! smoothed so producer-chunk granularity doesn't jitter the pitch.
//!
//! ## Fixed policies + counters (ADR-0038 §9 — know and say, never improvise)
//!
//! - **Ring empty → zeros**, counted per missing input frame (`input_ring_underruns`). The
//!   stage then re-enters warmup so a stalled input device re-primes cleanly — and the zeros
//!   delivered *while* re-priming are still counted (per callback, as its input-frame
//!   demand), so a glitching device's silence is fully accounted for. Only the initial
//!   startup prefill is free: silence before the ring has ever flowed is expected, not an
//!   underrun.
//! - **Ring full → drop oldest**, counted per dropped frame (`input_ring_overruns`): the
//!   consumer trims the *oldest* frames back down to the floor when fill crosses the
//!   high-water mark (floor + [`HIGH_WATER_SLACK_BLOCKS`] core blocks). Drop-oldest is
//!   necessarily a consumer-side act in an SPSC ring; if the consumer is stalled outright the
//!   producer's only possible move is to drop the *incoming* frame — the backstop, counted
//!   separately (`input_ring_producer_drops`) because it means the opposite diagnosis: a
//!   stalled output callback, not a rate mismatch.
//!
//! ## Latency budget (ring sizing, documented per P5)
//!
//! Added input latency on top of the device's own buffering:
//!
//! - ring floor: [`RING_FLOOR_BLOCKS`] (= 2) core blocks — ~10.7 ms at the 256/48k defaults;
//! - resampler lookahead: 1 input frame (~0.02 ms);
//! - P3's input staging in [`crate::engine::Engine::fill_duplex`]: 1 core block (~5.3 ms).
//!
//! Total ≈ 3 core blocks (~16 ms at defaults) — modest, and dominated by deliberate safety
//! margin: the floor must ride out the input device's own delivery granularity, and the
//! staging block is what makes the core pull causal. The ring's *capacity* is much larger
//! (floor + [`RING_HEADROOM_SECS`] of headroom) but capacity is not latency: an uncounted
//! bulk trim re-anchors fill at (demand + floor) every time warmup completes (startup, and
//! every re-prime), the servo holds it there, and the high-water trim bounds any excursion a
//! stalled output callback leaves behind at floor + [`HIGH_WATER_SLACK_BLOCKS`] blocks
//! (~96 ms at defaults) rather than letting it sit as sustained added latency.

use std::collections::BTreeMap;
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{SampleFormat, Stream};

use crate::audio::{find_named_device, for_each_duplicate_target, validate_map_pairs, AudioError};
use crate::diagnostics::Diagnostics;
use crate::profile::DeviceProfile;

/// Steady-state ring fill, in core blocks (converted to input-rate frames at setup): the
/// safety margin between the input device's delivery granularity and the output callback's
/// demand. Two blocks (~10.7 ms at defaults) rides out typical input callback chunk sizes;
/// the underrun counter says so if a rig needs more.
const RING_FLOOR_BLOCKS: usize = 2;

/// Ring headroom above the floor, in seconds of input audio. Capacity, not latency (see the
/// module doc): the slack that absorbs a late output callback or a startup burst before the
/// drop-oldest policy has to engage. Half a second of stereo f32 is ~192 KiB — preallocated
/// once, trivially cheap.
const RING_HEADROOM_SECS: f64 = 0.5;

/// Drop-oldest trigger height above the floor, in core blocks (converted to input-rate frames
/// at setup). Fill past `floor + this` is audio the ±0.5% servo can't plausibly drain (it
/// trims ~5 ms/s) sitting in the ring as pure added latency — an output stall's backlog, or a
/// rate mismatch beyond the servo's authority — so the counted drop-oldest trim re-anchors at
/// the floor instead. 16 blocks (~85 ms at defaults) is well above any real input device's
/// delivery granularity, so steady-state jitter never trips a counted trim, while bounding
/// worst-case sustained input latency at ~96 ms instead of the ~380 ms a ¾-capacity trigger
/// would allow.
const HIGH_WATER_SLACK_BLOCKS: usize = 16;

/// Ceiling on the *reported* input rate used for ring sizing: ~4× the fastest real capture
/// hardware (384 kHz). A garbage rate from a broken driver must not turn `rate ×`
/// [`RING_HEADROOM_SECS`] (or the ratio-scaled floor) into a multi-GB startup allocation.
/// Geometry only — the resampler's nominal ratio still follows the reported rate, and the §9
/// counters say so if that rate is genuinely garbage.
const MAX_GEOMETRY_INPUT_RATE: f64 = 1_536_000.0;

/// Open the input side (P5/#182): select the device the profile asks for, build the
/// device→logical [`InputMap`], size + allocate the SPSC ring, and build (not yet play) the
/// cpal input stream. Returns the stream (caller keeps it alive and `play()`s it once the
/// output side is running) and the [`InputStage`] the output callback pulls logical input
/// from.
///
/// Only called when the played instrument binds input channels (`logical_channels > 0`) — an
/// instrument without input pipes never touches an input device. Failures to *open* (no
/// device, no name match, unusable config, no f32-capable config, build error) are fatal
/// [`AudioError`]s: the instrument explicitly asked for live input, so silently playing
/// without a device would violate "know and say" (recorded as a deliberate ADR-0038 §7
/// carve-out). Channel-count mismatches between the device and the instrument are the
/// non-fatal path (§7): warn once, zero-fill, keep playing.
///
/// The input stream runs at the device's **default** config when that config is already f32;
/// a non-f32 default (i16 is common on ALSA) negotiates an f32 config from
/// `supported_input_configs` ([`find_f32_input_config`]) — like the output side, the format
/// is only fatal when the *hardware* genuinely has no f32 path. The profile's
/// `sample_rate`/`buffer_size` preferences are output-side (P4) — the resampler absorbs
/// whatever rate the input device runs at, which is the §8 design.
pub(crate) fn open_input(
    host: &cpal::Host,
    profile: &DeviceProfile,
    logical_channels: usize,
    engine_rate: f32,
    block_size: usize,
    diagnostics: Arc<Diagnostics>,
) -> Result<(Stream, InputStage), AudioError> {
    let device = select_input_device(host, profile.input.device.as_deref())?;
    let default_config = device
        .default_input_config()
        .map_err(AudioError::InputConfig)?;
    let supported = if default_config.sample_format() == SampleFormat::F32 {
        default_config
    } else {
        // The *default* isn't f32, but the device may still support it — consult the
        // supported configs before declaring the hardware unsupported (the output side's
        // `negotiate_rate` precedent).
        let configs: Vec<_> = device
            .supported_input_configs()
            .map_err(AudioError::InputSupportedConfigs)?
            .collect();
        let negotiated = find_f32_input_config(
            default_config.channels(),
            default_config.sample_rate().0,
            &configs,
        )
        .ok_or_else(|| AudioError::UnsupportedInputFormat(default_config.sample_format()))?;
        println!(
            "input device default format is {:?}; using a supported f32 config instead \
             ({} Hz, {} channel(s))",
            default_config.sample_format(),
            negotiated.sample_rate().0,
            negotiated.channels()
        );
        negotiated
    };
    if supported.channels() == 0 {
        // A 0-channel config would make the RT callback's per-frame iteration
        // (`chunks_exact(0)`) panic — refuse at open, where failing is allowed.
        return Err(AudioError::NoInputChannels);
    }
    let config: cpal::StreamConfig = supported.into();
    let device_channels = config.channels as usize;
    let in_rate = config.sample_rate.0 as f32;
    println!(
        "audio in @ {in_rate} Hz, {device_channels} device channel(s) -> {logical_channels} \
         logical input channel(s)"
    );

    let map = build_input_map(&profile.input.map, device_channels, logical_channels);
    let (floor, capacity, high_water) =
        ring_dimensions(block_size, in_rate as f64, engine_rate as f64);
    let (producer, consumer) = rtrb::RingBuffer::new(capacity * logical_channels);

    let stream = build_input_stream(
        &device,
        &config,
        producer,
        map,
        logical_channels,
        Arc::clone(&diagnostics),
    )?;
    let nominal_ratio = nominal_ratio(in_rate as f64, engine_rate as f64);
    let stage = InputStage::new(
        consumer,
        logical_channels,
        nominal_ratio,
        floor,
        high_water,
        capacity,
        diagnostics,
    );
    Ok((stream, stage))
}

/// Pure selection logic for [`open_input`]'s non-f32-default path (no device I/O, so it has
/// unit tests without a real [`cpal::Device`] — the shape of `audio.rs`'s `negotiate_rate`):
/// find an F32 config among `configs`, preferring one at the device default's channel count,
/// with the sample rate clamped as close to the default rate as the chosen range allows.
/// `None` means the device genuinely has no f32 path — only then is a non-f32 default fatal.
fn find_f32_input_config(
    default_channels: cpal::ChannelCount,
    default_rate: u32,
    configs: &[cpal::SupportedStreamConfigRange],
) -> Option<cpal::SupportedStreamConfig> {
    let f32s = || {
        configs
            .iter()
            .filter(|r| r.sample_format() == SampleFormat::F32)
    };
    let range = f32s()
        .find(|r| r.channels() == default_channels)
        .or_else(|| f32s().next())?;
    let rate = default_rate.clamp(range.min_sample_rate().0, range.max_sample_rate().0);
    Some(range.with_sample_rate(cpal::SampleRate(rate)))
}

/// Select an input device (ADR-0038 §6, the dual of `select_output_device`): `None` is the
/// host default; `Some(substr)` is the first input device whose name contains `substr`,
/// case-insensitively (the shared [`find_named_device`] kernel).
fn select_input_device(
    host: &cpal::Host,
    name_substr: Option<&str>,
) -> Result<cpal::Device, AudioError> {
    match name_substr {
        None => host.default_input_device().ok_or(AudioError::NoInputDevice),
        Some(substr) => {
            let devices = host
                .input_devices()
                .map_err(AudioError::InputDevicesQuery)?;
            find_named_device(devices, substr)
                .ok_or_else(|| AudioError::NoMatchingInputDevice(substr.to_string()))
        }
    }
}

/// Build the cpal input stream: the ring's producer side. Per device frame, apply the
/// device→logical map into a preallocated scratch frame, then commit it to the ring as one
/// whole frame ([`push_frame`]) — the consumer never sees a torn frame. Ring full is the
/// producer-side backstop (see the module doc): the incoming frame is dropped and counted in
/// `input_ring_producer_drops` — the stalled-output-callback diagnosis, distinct from the
/// consumer-side `input_ring_overruns` trim.
fn build_input_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut producer: rtrb::Producer<f32>,
    map: InputMap,
    logical_channels: usize,
    diagnostics: Arc<Diagnostics>,
) -> Result<Stream, AudioError> {
    let device_channels = config.channels as usize;
    // Preallocated once; the callback only writes into it. Never grows.
    let mut logical_frame = vec![0.0f32; logical_channels];
    device
        .build_input_stream(
            config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // `chunks_exact` so a ragged tail (never expected from cpal, but not our
                // panic to risk on the RT thread) is dropped rather than smeared across
                // channels.
                for device_frame in data.chunks_exact(device_channels) {
                    map.apply(device_frame, &mut logical_frame);
                    if !push_frame(&mut producer, &logical_frame) {
                        diagnostics.record_input_ring_producer_drop_frames(1);
                    }
                }
            },
            |err| eprintln!("audio input stream error: {err}"),
            None,
        )
        .map_err(AudioError::BuildInput)
}

/// Commit one whole logical frame to the ring, atomically from the consumer's point of view:
/// either every sample of the frame becomes readable or none does (a torn frame would
/// permanently misalign channel interleaving for the rest of the run). Returns `false` —
/// frame dropped — when the ring can't take a whole frame.
fn push_frame(producer: &mut rtrb::Producer<f32>, frame: &[f32]) -> bool {
    match producer.write_chunk(frame.len()) {
        Ok(mut chunk) => {
            let (a, b) = chunk.as_mut_slices();
            let split = a.len();
            a.copy_from_slice(&frame[..split]);
            b.copy_from_slice(&frame[split..]);
            chunk.commit_all();
            true
        }
        Err(_) => false,
    }
}

/// Pop one whole logical frame; `false` (frame untouched by contract of the caller — the
/// resampler substitutes silence) when a whole frame isn't available. The producer only
/// commits whole frames, so a whole-frame `read_chunk` cannot tear — and the chunk read costs
/// one Release commit per frame instead of one per sample.
fn pop_frame(consumer: &mut rtrb::Consumer<f32>, frame: &mut [f32]) -> bool {
    match consumer.read_chunk(frame.len()) {
        Ok(chunk) => {
            let (a, b) = chunk.as_slices();
            frame[..a.len()].copy_from_slice(a);
            frame[a.len()..].copy_from_slice(b);
            chunk.commit_all();
            true
        }
        Err(_) => false,
    }
}

/// The nominal input-frames-per-engine-frame ratio, guarded against garbage device rates the
/// same way `audio.rs`'s `callback_budget` is: a non-finite or non-positive rate can't crash
/// the callback, it just degrades to 1.0 (and the servo then does what it can).
fn nominal_ratio(in_rate: f64, engine_rate: f64) -> f64 {
    if in_rate.is_finite() && in_rate > 0.0 && engine_rate.is_finite() && engine_rate > 0.0 {
        in_rate / engine_rate
    } else {
        1.0
    }
}

/// Ring geometry in **input-rate frames**: `(floor, capacity, high_water)`.
///
/// - `floor`: [`RING_FLOOR_BLOCKS`] core blocks converted to input-rate frames — the
///   steady-state fill the servo holds and the added latency the ring costs (module doc).
/// - `capacity`: floor + [`RING_HEADROOM_SECS`] of input audio (never less than 8 core
///   blocks, so a garbage device rate still yields a workable ring).
/// - `high_water`: floor + [`HIGH_WATER_SLACK_BLOCKS`] core blocks (clamped below ¾ of
///   capacity so degenerate small rings keep a working trigger) — crossing it means the servo
///   lost (a rate mismatch beyond ±0.5%, or an output stall's backlog) and the drop-oldest
///   trim engages, bounding sustained added latency near the floor instead of at capacity.
///
/// Every dimension derives from the reported rate clamped to [`MAX_GEOMETRY_INPUT_RATE`], so
/// a garbage rate can't demand a multi-GB preallocation.
fn ring_dimensions(block_size: usize, in_rate: f64, engine_rate: f64) -> (usize, usize, usize) {
    let geometry_rate = if in_rate.is_finite() && in_rate > 0.0 {
        in_rate.min(MAX_GEOMETRY_INPUT_RATE)
    } else {
        0.0 // degrades to ratio 1.0 + block-derived headroom below
    };
    let ratio = nominal_ratio(geometry_rate, engine_rate);
    let floor = ((RING_FLOOR_BLOCKS * block_size) as f64 * ratio)
        .ceil()
        .max(1.0) as usize;
    let headroom_frames = if geometry_rate > 0.0 {
        (geometry_rate * RING_HEADROOM_SECS).ceil() as usize
    } else {
        0
    };
    let capacity = floor + headroom_frames.max(8 * block_size);
    let slack = ((HIGH_WATER_SLACK_BLOCKS * block_size) as f64 * ratio)
        .ceil()
        .max(1.0) as usize;
    let high_water = (floor + slack).min(capacity - capacity / 4);
    (floor, capacity, high_water)
}

/// The device→logical input channel map (ADR-0038 §6/§7): the dual of `audio.rs`'s output
/// mapping. `Identity` is the no-profile default — logical channel `c` reads device channel
/// `c`, and a logical channel past the device's width reads silence (warned once at setup).
/// `Explicit` is a profile's validated `input.map`, which overrides the identity policy
/// entirely. Validated once at setup, never re-checked per frame.
enum InputMap {
    Identity,
    Explicit {
        /// Validated `(device, logical)` pairs, ascending device order — both indices already
        /// checked in range.
        pairs: Vec<(usize, usize)>,
        /// `true` at index `l` for every logical channel a pair feeds; the rest read silence
        /// (§7), zeroed without re-deriving this per frame.
        fed: Vec<bool>,
    },
}

impl InputMap {
    /// Apply the map to one device frame, filling one logical frame. Allocation-free; every
    /// index was validated at build time.
    fn apply(&self, device_frame: &[f32], logical_frame: &mut [f32]) {
        match self {
            InputMap::Identity => {
                for (c, out) in logical_frame.iter_mut().enumerate() {
                    *out = device_frame.get(c).copied().unwrap_or(0.0);
                }
            }
            InputMap::Explicit { pairs, fed } => {
                for (l, out) in logical_frame.iter_mut().enumerate() {
                    if !fed[l] {
                        *out = 0.0;
                    }
                }
                for &(d, l) in pairs {
                    logical_frame[l] = device_frame.get(d).copied().unwrap_or(0.0);
                }
            }
        }
    }
}

/// Build the active input map from a profile's `input.map` (device→logical, ADR-0038 §6).
/// Empty map = [`InputMap::Identity`]. All mismatch handling is §7 warn+degrade, emitted once
/// here at setup (the "one-time startup warning"):
///
/// - identity with a device narrower than the instrument's input width → the unsupplied
///   logical channels read silence;
/// - a pair naming a device or logical channel that doesn't exist → dropped;
/// - two device channels feeding the same logical channel → both kept, applied in ascending
///   device order, so the highest device channel wins (named, not accidental — the same
///   [`for_each_duplicate_target`] kernel as the output side, so the collision rule can't
///   drift between the two);
/// - a logical channel no pair feeds → reads silence.
///
/// Validation + mask building is `audio.rs`'s shared [`validate_map_pairs`] kernel; only the
/// warning wording is input-side.
fn build_input_map(map: &BTreeMap<usize, usize>, device: usize, logical: usize) -> InputMap {
    if map.is_empty() {
        if logical > device {
            eprintln!(
                "warning: instrument binds {logical} logical input channel(s) but the input \
                 device supplies {device}; logical input channel(s) {device}..={} read silence",
                logical - 1
            );
        }
        return InputMap::Identity;
    }
    let (pairs, fed) = validate_map_pairs(
        map,
        device,
        logical,
        |d| {
            eprintln!(
                "warning: io-map input.map reads device channel {d}, but the device has \
                 {device} channel(s); dropped"
            );
        },
        |l| {
            eprintln!(
                "warning: io-map input.map feeds logical input channel {l}, but the instrument \
                 binds {logical}; dropped"
            );
        },
    );
    for_each_duplicate_target(&pairs, |l, devices, winner| {
        eprintln!(
            "warning: io-map input.map feeds logical input channel {l} from multiple device \
             channels {devices:?}; device channel {winner} wins (applied last), the rest \
             are dropped for that logical channel"
        );
    });
    for (l, is_fed) in fed.iter().enumerate() {
        if !is_fed {
            eprintln!(
                "warning: io-map input.map feeds no device channel into logical input channel \
                 {l}; it reads silence"
            );
        }
    }
    InputMap::Explicit { pairs, fed }
}

/// Linear-interpolation resampler over interleaved frames (the recorded P5 choice — see the
/// module doc). Holds a two-frame window (`prev`, `cur` = source frames `s[j]`, `s[j+1]`) and
/// a fractional `phase` ∈ [0, 1); each output frame emits `lerp(prev, cur, phase)` and
/// advances `phase` by the ratio (input frames per output frame), pulling source frames as
/// the window slides. RT-safe: state is two preallocated frames; `process` never allocates.
///
/// At ratio 1.0 the phase lands on exactly 0 every frame (f64 arithmetic on 1.0 is exact), so
/// equal-rate passthrough is **bit-exact**. The window means one input frame of lookahead —
/// counted in the module doc's latency budget.
struct LinearResampler {
    channels: usize,
    /// Fractional read position between `prev` and `cur`. Starts at 2.0 so the first output
    /// frame pulls both window frames and emits source frame 0 exactly.
    phase: f64,
    prev: Vec<f32>,
    cur: Vec<f32>,
}

impl LinearResampler {
    /// The most source frames one [`Self::process`] call can pull beyond
    /// `ceil(out_frames × ratio)`: one for the window prime (the first output frame pulls
    /// both window frames) and one for the fractional phase remainder. Exposed so demand
    /// sizing ([`InputStage::fill`]) tracks the resampler's actual contract instead of
    /// hardcoding window internals a swapped-in resampler (module doc) wouldn't share.
    const MAX_EXTRA_SOURCE_FRAMES: usize = 2;

    fn new(channels: usize) -> Self {
        Self {
            channels,
            phase: 2.0,
            prev: vec![0.0; channels],
            cur: vec![0.0; channels],
        }
    }

    /// Back to the just-built state (used when the stage re-enters warmup after an underrun,
    /// so the re-primed stream starts from a clean window instead of a stale one).
    fn reset(&mut self) {
        self.phase = 2.0;
        self.prev.fill(0.0);
        self.cur.fill(0.0);
    }

    /// Fill `out` (interleaved at [`Self::channels`]) at `ratio` input frames per output
    /// frame, pulling source frames from `next` (which writes one frame and returns `true`,
    /// or returns `false` when dry). A dry pull slides silence into the window — the ADR §9
    /// empty→zeros policy, with a one-sample interpolated fade instead of a hard edge — and
    /// is counted; the return value is the number of missing source frames.
    fn process(
        &mut self,
        ratio: f64,
        out: &mut [f32],
        mut next: impl FnMut(&mut [f32]) -> bool,
    ) -> u64 {
        debug_assert_eq!(out.len() % self.channels, 0);
        let mut missed = 0u64;
        for frame in out.chunks_exact_mut(self.channels) {
            while self.phase >= 1.0 {
                self.phase -= 1.0;
                std::mem::swap(&mut self.prev, &mut self.cur);
                if !next(&mut self.cur) {
                    self.cur.fill(0.0);
                    missed += 1;
                }
            }
            if self.phase == 0.0 {
                // The exact-passthrough path (ratio 1.0, or any integer landing): no lerp
                // rounding, `prev` verbatim.
                frame.copy_from_slice(&self.prev);
            } else {
                for (c, s) in frame.iter_mut().enumerate() {
                    let a = self.prev[c] as f64;
                    let b = self.cur[c] as f64;
                    *s = (a + (b - a) * self.phase) as f32;
                }
            }
            self.phase += ratio;
        }
        missed
    }
}

/// The ring-fill-level servo (module doc): steers the resample ratio so the ring's residual
/// fill (measured after each output callback consumes) holds at `floor` input frames.
/// Proportional control, error normalized by the floor and clamped to ±1, correction clamped
/// to ±[`Self::MAX_CORRECTION`] and one-pole smoothed.
struct DriftServo {
    /// Target residual, input frames.
    floor: f64,
    /// Smoothed dimensionless correction, applied as `nominal * (1 + correction)`.
    correction: f64,
}

impl DriftServo {
    /// Correction ceiling: ±0.5%. An order of magnitude above real-world clock drift
    /// (typically ≤ ±0.02%), so the servo always has authority, while staying inaudible
    /// (≈ 8.6 cents at full deflection, only ever reached transiently).
    const MAX_CORRECTION: f64 = 0.005;
    /// One-pole smoothing per update (one update per output callback): heavy enough that
    /// producer-chunk granularity in the residual measurement doesn't wobble the pitch,
    /// light enough to converge in well under a second at typical callback rates.
    const SMOOTHING: f64 = 0.1;

    fn new(floor: usize) -> Self {
        Self {
            floor: (floor.max(1)) as f64,
            correction: 0.0,
        }
    }

    /// The ratio to resample this callback at.
    fn ratio(&self, nominal: f64) -> f64 {
        nominal * (1.0 + self.correction)
    }

    /// Feed one post-consumption residual measurement (input frames).
    fn update(&mut self, residual_frames: usize) {
        let err = ((residual_frames as f64 - self.floor) / self.floor).clamp(-1.0, 1.0);
        let target = err * Self::MAX_CORRECTION;
        self.correction += Self::SMOOTHING * (target - self.correction);
    }

    /// Forget accumulated correction (used when the stage re-enters warmup — the old drift
    /// estimate may be exactly what starved the ring).
    fn reset(&mut self) {
        self.correction = 0.0;
    }
}

/// The consumer half of the input path, owned by the **output** callback: pulls device-rate
/// logical frames off the ring and resamples them into the engine-rate interleaved input
/// block `Engine::fill_duplex` consumes. All policy (warmup prefill, drop-oldest trim,
/// empty→zeros) lives in [`InputStage::fill`]; everything is preallocated at setup.
pub struct InputStage {
    consumer: rtrb::Consumer<f32>,
    channels: usize,
    nominal_ratio: f64,
    resampler: LinearResampler,
    servo: DriftServo,
    /// Target residual fill, input frames (the latency the ring costs — module doc).
    floor: usize,
    /// Drop-oldest trigger, input frames (floor + [`HIGH_WATER_SLACK_BLOCKS`] blocks).
    high_water: usize,
    /// Ring capacity, input frames: the warmup gate's ceiling — a demand the ring can't
    /// physically hold must still warm once the ring is full, or the stage would sit in
    /// permanent uncounted silence while the producer drops every incoming frame.
    capacity: usize,
    /// `false` until the ring has prefilled to (this callback's demand + floor, capped at
    /// capacity); while warming, output is zeros.
    warmed: bool,
    /// `true` once the ring has flowed at all. Gates underrun counting during warmup: initial
    /// prefill silence is expected (uncounted), but silence while *re*-priming after a dry
    /// spell is the device still failing to deliver — counted (module doc, §9).
    ever_warmed: bool,
    diagnostics: Arc<Diagnostics>,
}

impl InputStage {
    fn new(
        consumer: rtrb::Consumer<f32>,
        channels: usize,
        nominal_ratio: f64,
        floor: usize,
        high_water: usize,
        capacity: usize,
        diagnostics: Arc<Diagnostics>,
    ) -> Self {
        Self {
            consumer,
            channels,
            nominal_ratio,
            resampler: LinearResampler::new(channels),
            servo: DriftServo::new(floor),
            floor,
            high_water,
            capacity,
            warmed: false,
            ever_warmed: false,
            diagnostics,
        }
    }

    /// Fill `out` (interleaved logical input at the engine rate, `frames * channels` long)
    /// from the ring. Called once per output callback, on the output thread. RT-safe: no
    /// allocation, no locks; the ring ops are wait-free, and every bulk trim is O(1)
    /// ([`Self::discard_frames`]).
    pub(crate) fn fill(&mut self, out: &mut [f32]) {
        let frames = out.len() / self.channels;
        let ratio = self.servo.ratio(self.nominal_ratio);
        // Input frames this callback will pull: the resampled demand plus the resampler's
        // documented worst-case extra pulls.
        let demand = (frames as f64 * ratio).ceil() as usize;
        let need = demand + LinearResampler::MAX_EXTRA_SOURCE_FRAMES;
        let avail = self.consumer.slots() / self.channels;

        if !self.warmed {
            // Warmup gate, capped at capacity: a granted output buffer so large that
            // `need + floor` can't fit in the ring must still warm once the ring is full —
            // otherwise the stage never flows at all (permanent uncounted silence plus an
            // unbounded producer-drop count, the exact avalanche §9 forbids).
            let target = (need + self.floor).min(self.capacity);
            if avail < target {
                out.fill(0.0);
                // Startup prefill is free; re-prime silence is a counted underrun (see
                // `ever_warmed`), sized as this callback's input-frame demand.
                if self.ever_warmed {
                    self.diagnostics
                        .record_input_ring_underrun_frames(demand as u64);
                }
                return;
            }
            self.warmed = true;
            self.ever_warmed = true;
            // Re-anchor latency at the floor (module doc): anything the ring accumulated
            // beyond this callback's demand + floor — early capture on backends that buffer
            // before `play`, or the backlog behind the warmup threshold itself — is buffered
            // latency nobody has heard yet. Trim it now, uncounted, so steady state *starts*
            // at the floor instead of leaving the ±0.5% servo to drain it at ~5 ms/s.
            self.discard_frames(avail.saturating_sub(need + self.floor));
        } else if avail > self.high_water {
            // Drop-oldest (ADR-0038 §9): past the high-water mark the servo has lost —
            // discard the *oldest* frames down to exactly what this callback needs plus the
            // floor, count every dropped frame, and let the servo re-center from there.
            let drop_frames = avail.saturating_sub(need + self.floor);
            if drop_frames > 0 {
                self.discard_frames(drop_frames);
                self.diagnostics
                    .record_input_ring_overrun_frames(drop_frames as u64);
            }
        }

        let (resampler, consumer) = (&mut self.resampler, &mut self.consumer);
        let missed = resampler.process(ratio, out, |frame| pop_frame(consumer, frame));
        if missed > 0 {
            self.diagnostics.record_input_ring_underrun_frames(missed);
            // Ran dry: re-enter warmup for a clean re-prime (fresh window, fresh drift
            // estimate). Re-prime silence keeps counting (module doc) — only the *initial*
            // prefill is free.
            self.warmed = false;
            self.resampler.reset();
            self.servo.reset();
            return;
        }
        let residual = self.consumer.slots() / self.channels;
        self.servo.update(residual);
    }

    /// Discard `frames` whole frames from the front of the ring (the drop-oldest act) in
    /// O(1): one wait-free chunk claim + one Release commit, regardless of `frames` — a
    /// per-sample pop loop here would put tens of thousands of atomic ops on the output
    /// thread exactly when the system is already stressed. The producer only commits whole
    /// frames, so discarding whole frames keeps channel alignment.
    fn discard_frames(&mut self, frames: usize) {
        let samples = (frames * self.channels).min(self.consumer.slots());
        if let Ok(chunk) = self.consumer.read_chunk(samples) {
            chunk.commit_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- InputMap -----------------------------------------------------------------------

    #[test]
    fn identity_map_copies_matching_channels() {
        let map = build_input_map(&BTreeMap::new(), 2, 2);
        let mut logical = [9.0f32; 2];
        map.apply(&[0.25, -0.5], &mut logical);
        assert_eq!(logical, [0.25, -0.5]);
    }

    #[test]
    fn identity_map_zero_fills_logical_channels_past_device_width() {
        // ADR-0038 §7: a mono mic under a stereo-input instrument supplies channel 0;
        // channel 1 reads silence (warned once at setup).
        let map = build_input_map(&BTreeMap::new(), 1, 2);
        let mut logical = [9.0f32; 2];
        map.apply(&[0.75], &mut logical);
        assert_eq!(logical, [0.75, 0.0]);
    }

    #[test]
    fn explicit_map_routes_device_to_logical_and_zero_fills_unfed() {
        // Device channel 2 -> logical 0; logical 1 is fed by nothing -> silence.
        let mut m = BTreeMap::new();
        m.insert(2usize, 0usize);
        let map = build_input_map(&m, 4, 2);
        let mut logical = [9.0f32; 2];
        map.apply(&[0.1, 0.2, 0.3, 0.4], &mut logical);
        assert_eq!(logical, [0.3, 0.0]);
    }

    #[test]
    fn explicit_map_drops_out_of_range_pairs() {
        let mut m = BTreeMap::new();
        m.insert(7usize, 0usize); // device only has 2 channels
        m.insert(0usize, 9usize); // instrument only binds 2 logical inputs
        let map = build_input_map(&m, 2, 2);
        match &map {
            InputMap::Explicit { pairs, .. } => assert!(pairs.is_empty(), "bad pairs kept"),
            InputMap::Identity => panic!("non-empty map must build Explicit"),
        }
        // Nothing fed -> all logical channels read silence.
        let mut logical = [9.0f32; 2];
        map.apply(&[0.1, 0.2], &mut logical);
        assert_eq!(logical, [0.0, 0.0]);
    }

    #[test]
    fn duplicate_logical_targets_apply_in_ascending_device_order() {
        // Device 0 and device 1 both feed logical 0: highest device channel wins (applied
        // last), mirroring the output side's collision rule.
        let mut m = BTreeMap::new();
        m.insert(0usize, 0usize);
        m.insert(1usize, 0usize);
        let map = build_input_map(&m, 2, 1);
        let mut logical = [9.0f32; 1];
        map.apply(&[0.1, 0.2], &mut logical);
        assert_eq!(logical, [0.2]);
    }

    // --- LinearResampler ----------------------------------------------------------------

    /// A test source over a fixed sample table: hands out consecutive frames, then goes dry.
    fn source_from(table: Vec<f32>, channels: usize) -> impl FnMut(&mut [f32]) -> bool {
        let mut next = 0usize;
        move |frame: &mut [f32]| {
            if next + channels > table.len() {
                return false;
            }
            frame.copy_from_slice(&table[next..next + channels]);
            next += channels;
            true
        }
    }

    #[test]
    fn ratio_one_is_bit_exact_passthrough() {
        // The dominant case (equal rates, servo centered): straight copies, no lerp rounding.
        let table: Vec<f32> = (0..32).map(|i| (i as f32 * 0.37).sin()).collect();
        let mut r = LinearResampler::new(1);
        let mut out = vec![0.0f32; 16];
        let missed = r.process(1.0, &mut out, source_from(table.clone(), 1));
        assert_eq!(missed, 0);
        for (i, (got, want)) in out.iter().zip(&table).enumerate() {
            assert_eq!(got.to_bits(), want.to_bits(), "sample {i}");
        }
    }

    #[test]
    fn linear_ramp_is_reproduced_exactly_at_any_ratio() {
        // Linear interpolation is exact on linear signals: with source s[k] = k, output n
        // must be n * ratio (within f32 rounding), for a non-trivial ratio like 44100/48000.
        let ratio = 44_100.0f64 / 48_000.0;
        let table: Vec<f32> = (0..64).map(|k| k as f32).collect();
        let mut r = LinearResampler::new(1);
        let mut out = vec![0.0f32; 48];
        let missed = r.process(ratio, &mut out, source_from(table, 1));
        assert_eq!(missed, 0);
        for (n, &got) in out.iter().enumerate() {
            let want = (n as f64 * ratio) as f32;
            assert!(
                (got - want).abs() < 1e-4,
                "output {n}: got {got}, want {want}"
            );
        }
    }

    #[test]
    fn ratio_two_downsamples_every_other_frame() {
        let table: Vec<f32> = (0..32).map(|k| k as f32).collect();
        let mut r = LinearResampler::new(1);
        let mut out = vec![0.0f32; 8];
        let missed = r.process(2.0, &mut out, source_from(table, 1));
        assert_eq!(missed, 0);
        for (n, &got) in out.iter().enumerate() {
            assert_eq!(got, (2 * n) as f32, "output {n}");
        }
    }

    #[test]
    fn channels_stay_aligned_through_resampling() {
        // Stereo frames with distinct per-channel values must never swap channels.
        let frames = 32;
        let mut table = Vec::with_capacity(frames * 2);
        for k in 0..frames {
            table.push(k as f32); // left: ramp
            table.push(-(k as f32)); // right: negated ramp
        }
        let mut r = LinearResampler::new(2);
        let mut out = vec![0.0f32; 20 * 2];
        let missed = r.process(1.25, &mut out, source_from(table, 2));
        assert_eq!(missed, 0);
        for frame in out.chunks_exact(2) {
            assert_eq!(
                frame[0].to_bits(),
                (-frame[1]).to_bits(),
                "channels desynced: {frame:?}"
            );
        }
    }

    #[test]
    fn dry_source_yields_zeros_and_counts_misses() {
        // 4 source frames, 8 output frames wanted at ratio 1: frame 0 pulls two (window
        // prime), frames 1..3 pull one each -> the source is exhausted after output frame 3;
        // outputs 4.. slide silence in and count a miss per pull.
        let table: Vec<f32> = vec![1.0, 2.0, 3.0, 4.0];
        let mut r = LinearResampler::new(1);
        let mut out = vec![9.0f32; 8];
        let missed = r.process(1.0, &mut out, source_from(table, 1));
        assert_eq!(missed, 5);
        assert_eq!(&out[..4], &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(&out[4..], &[0.0; 4]);
    }

    #[test]
    fn reset_restores_the_fresh_window() {
        let mut r = LinearResampler::new(1);
        let mut out = vec![0.0f32; 4];
        r.process(1.0, &mut out, source_from(vec![5.0, 6.0, 7.0, 8.0, 9.0], 1));
        r.reset();
        let missed = r.process(1.0, &mut out, source_from(vec![1.0, 2.0, 3.0, 4.0, 5.0], 1));
        assert_eq!(missed, 0);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0], "stale window survived reset");
    }

    // --- DriftServo ---------------------------------------------------------------------

    #[test]
    fn servo_speeds_up_when_fill_is_above_floor() {
        let mut s = DriftServo::new(512);
        s.update(1024); // surplus -> consume input faster -> ratio above nominal
        assert!(s.ratio(1.0) > 1.0);
    }

    #[test]
    fn servo_slows_down_when_fill_is_below_floor() {
        let mut s = DriftServo::new(512);
        s.update(0); // starving -> consume slower -> ratio below nominal
        assert!(s.ratio(1.0) < 1.0);
    }

    #[test]
    fn servo_correction_is_clamped_and_converges() {
        // A wildly overfull ring converges to exactly +MAX_CORRECTION, never beyond.
        let mut s = DriftServo::new(512);
        for _ in 0..1000 {
            s.update(100_000);
            assert!(s.ratio(1.0) <= 1.0 + DriftServo::MAX_CORRECTION + 1e-12);
        }
        assert!((s.ratio(1.0) - (1.0 + DriftServo::MAX_CORRECTION)).abs() < 1e-9);
        // And an empty ring converges to exactly -MAX_CORRECTION.
        for _ in 0..1000 {
            s.update(0);
        }
        assert!((s.ratio(1.0) - (1.0 - DriftServo::MAX_CORRECTION)).abs() < 1e-9);
    }

    #[test]
    fn servo_holds_nominal_at_the_floor_and_reset_clears_drift() {
        let mut s = DriftServo::new(512);
        s.update(512);
        assert_eq!(s.ratio(1.0), 1.0, "at-floor residual must not correct");
        s.update(2048);
        assert!(s.ratio(1.0) > 1.0);
        s.reset();
        assert_eq!(s.ratio(1.0), 1.0);
    }

    // --- Ring geometry ------------------------------------------------------------------

    #[test]
    fn ring_dimensions_match_the_documented_budget() {
        // Defaults: 256-frame blocks, both sides at 48 kHz. Floor = 2 blocks = 512 input
        // frames (~10.7 ms); capacity = floor + 0.5 s; high water at floor + 16 blocks —
        // the latency bound, far below ¾ capacity.
        let (floor, capacity, high_water) = ring_dimensions(256, 48_000.0, 48_000.0);
        assert_eq!(floor, 512);
        assert_eq!(capacity, 512 + 24_000);
        assert_eq!(high_water, 512 + 16 * 256);
        assert!(floor < high_water && high_water < capacity);
    }

    #[test]
    fn ring_dimensions_scale_the_floor_with_the_rate_ratio() {
        // A 96 kHz input into a 48 kHz engine needs twice the input frames per core block.
        let (floor, _, _) = ring_dimensions(256, 96_000.0, 48_000.0);
        assert_eq!(floor, 1024);
        // And a garbage input rate degrades to ratio 1.0 with block-derived headroom, never
        // a panic or a zero-capacity ring.
        let (floor, capacity, high_water) = ring_dimensions(256, 0.0, 48_000.0);
        assert_eq!(floor, 512);
        assert_eq!(capacity, 512 + 8 * 256);
        assert!(high_water < capacity);
    }

    #[test]
    fn ring_dimensions_cap_a_garbage_huge_rate() {
        // A broken driver reporting a multi-GHz rate must not demand a multi-GB ring: the
        // geometry rate is clamped to MAX_GEOMETRY_INPUT_RATE (the resampler's nominal ratio
        // is unaffected — the §9 counters surface the consequences instead).
        let (floor, capacity, high_water) = ring_dimensions(256, 4.0e9, 48_000.0);
        let (want_floor, want_capacity, want_high_water) =
            ring_dimensions(256, MAX_GEOMETRY_INPUT_RATE, 48_000.0);
        assert_eq!(
            (floor, capacity, high_water),
            (want_floor, want_capacity, want_high_water)
        );
        assert!(capacity < 1_000_000, "capacity {capacity} not capped");
        // The degenerate-small-ring clamp: high water always stays below capacity.
        assert!(high_water < capacity);
    }

    // --- Input format negotiation (pure — no cpal device) ---------------------------------

    fn range(
        channels: cpal::ChannelCount,
        min: u32,
        max: u32,
        format: SampleFormat,
    ) -> cpal::SupportedStreamConfigRange {
        cpal::SupportedStreamConfigRange::new(
            channels,
            cpal::SampleRate(min),
            cpal::SampleRate(max),
            cpal::SupportedBufferSize::Range { min: 64, max: 4096 },
            format,
        )
    }

    #[test]
    fn f32_negotiation_prefers_the_default_channel_count() {
        // An i16-default ALSA device that also supports f32 (the review's case) must
        // negotiate, not refuse — and prefer the default config's channel count.
        let configs = vec![
            range(1, 44_100, 48_000, SampleFormat::F32),
            range(2, 44_100, 48_000, SampleFormat::I16),
            range(2, 44_100, 48_000, SampleFormat::F32),
        ];
        let cfg = find_f32_input_config(2, 48_000, &configs).expect("device supports f32");
        assert_eq!(cfg.channels(), 2);
        assert_eq!(cfg.sample_rate().0, 48_000);
        assert_eq!(cfg.sample_format(), SampleFormat::F32);
    }

    #[test]
    fn f32_negotiation_falls_back_to_another_channel_count() {
        let configs = vec![
            range(2, 44_100, 48_000, SampleFormat::I16),
            range(1, 44_100, 48_000, SampleFormat::F32),
        ];
        let cfg = find_f32_input_config(2, 48_000, &configs).expect("mono f32 exists");
        assert_eq!(cfg.channels(), 1);
        assert_eq!(cfg.sample_rate().0, 48_000);
    }

    #[test]
    fn f32_negotiation_clamps_the_rate_into_the_chosen_range() {
        // The default rate isn't inside the f32 range: stay as close as the range allows
        // (the resampler absorbs whatever rate results — §8).
        let configs = vec![range(2, 88_200, 96_000, SampleFormat::F32)];
        let cfg = find_f32_input_config(2, 48_000, &configs).expect("f32 exists");
        assert_eq!(cfg.sample_rate().0, 88_200);
    }

    #[test]
    fn f32_negotiation_reports_a_truly_f32_less_device() {
        // Only now is the fatal UnsupportedInputFormat honest about the hardware.
        let configs = vec![range(2, 44_100, 48_000, SampleFormat::I16)];
        assert!(find_f32_input_config(2, 48_000, &configs).is_none());
        assert!(find_f32_input_config(2, 48_000, &[]).is_none());
    }

    // --- Ring frame helpers -------------------------------------------------------------

    #[test]
    fn push_frame_is_all_or_nothing() {
        // Capacity for exactly two stereo frames: the third push must drop whole, leaving
        // channel alignment intact.
        let (mut p, mut c) = rtrb::RingBuffer::<f32>::new(4);
        assert!(push_frame(&mut p, &[1.0, 2.0]));
        assert!(push_frame(&mut p, &[3.0, 4.0]));
        assert!(!push_frame(&mut p, &[5.0, 6.0]), "full ring must drop");
        let mut frame = [0.0f32; 2];
        assert!(pop_frame(&mut c, &mut frame));
        assert_eq!(frame, [1.0, 2.0]);
        assert!(pop_frame(&mut c, &mut frame));
        assert_eq!(frame, [3.0, 4.0]);
        assert!(!pop_frame(&mut c, &mut frame), "empty ring must report dry");
    }

    #[test]
    fn frames_survive_the_ring_wrap_intact() {
        // The split-copy path in push_frame/pop_frame: a frame straddling the ring's
        // physical wrap point makes write_chunk/read_chunk hand back *two* non-empty
        // slices, and the frame must be reassembled across them in order — a swapped
        // sub-slice or an off-by-one at `split` misaligns channel interleaving for the
        // rest of the run (the module's central invariant). Odd capacity forces the
        // straddle: with an even capacity, stereo frames always land frame-aligned at
        // the wrap and the second slice stays empty.
        let (mut p, mut c) = rtrb::RingBuffer::<f32>::new(5);
        assert!(push_frame(&mut p, &[1.0, 2.0])); // slots 0-1
        assert!(push_frame(&mut p, &[3.0, 4.0])); // slots 2-3
        let mut frame = [0.0f32; 2];
        assert!(pop_frame(&mut c, &mut frame)); // read index now 2
        assert_eq!(frame, [1.0, 2.0]);
        // Write index 4: this frame splits — left sample in slot 4, right sample wrapped
        // to slot 0 (the two-slice WRITE path).
        assert!(push_frame(&mut p, &[5.0, 6.0]));
        assert!(pop_frame(&mut c, &mut frame));
        assert_eq!(frame, [3.0, 4.0]);
        // Read index 4: the two-slice READ path must put the pre-wrap sample at frame[0]
        // and the wrapped one at frame[1].
        assert!(pop_frame(&mut c, &mut frame));
        assert_eq!(frame, [5.0, 6.0]);
        // Contiguous again past the wrap (slots 1-2): alignment survived the straddle.
        assert!(push_frame(&mut p, &[7.0, 8.0]));
        assert!(pop_frame(&mut c, &mut frame));
        assert_eq!(frame, [7.0, 8.0]);
    }

    // --- InputStage (whole-policy tests over a real ring — no device needed) -------------

    /// A stage over a real rtrb ring with small, legible geometry: mono, ratio 1.0,
    /// floor 8 frames, high water 48, capacity 64.
    fn test_stage() -> (rtrb::Producer<f32>, InputStage, Arc<Diagnostics>) {
        let (producer, consumer) = rtrb::RingBuffer::new(64);
        let diagnostics = Diagnostics::new();
        let stage = InputStage::new(consumer, 1, 1.0, 8, 48, 64, Arc::clone(&diagnostics));
        (producer, stage, diagnostics)
    }

    fn push_samples(p: &mut rtrb::Producer<f32>, range: std::ops::Range<usize>) {
        for k in range {
            assert!(push_frame(p, &[k as f32]), "test ring overflow at {k}");
        }
    }

    #[test]
    fn warmup_outputs_zeros_and_counts_nothing() {
        let (_p, mut stage, diag) = test_stage();
        let mut out = [9.0f32; 16];
        stage.fill(&mut out);
        assert_eq!(out, [0.0; 16], "prefill must read silence");
        assert_eq!(
            diag.snapshot().input_ring_underruns,
            0,
            "warmup is not an underrun"
        );
        assert_eq!(diag.snapshot().input_ring_overruns, 0);
    }

    #[test]
    fn warmed_stage_passes_input_through_bit_exact_at_ratio_one() {
        let (mut p, mut stage, diag) = test_stage();
        // Warmup demand for 16 frames at ratio 1.0: 16 + 2 (window prime) + 8 (floor) = 26.
        push_samples(&mut p, 0..26);
        let mut out = [9.0f32; 16];
        stage.fill(&mut out);
        for (n, &got) in out.iter().enumerate() {
            assert_eq!(got, n as f32, "frame {n}");
        }
        assert_eq!(diag.snapshot().input_ring_underruns, 0);
        assert_eq!(diag.snapshot().input_ring_overruns, 0);
    }

    #[test]
    fn dry_ring_counts_underruns_and_counts_reprime_silence() {
        let (mut p, mut stage, diag) = test_stage();
        push_samples(&mut p, 0..26);
        let mut out = [9.0f32; 16];
        stage.fill(&mut out); // consumes 17 (16 + window prime), residual 9

        // No new input: the next 16-frame pull has only 9 frames -> 7 missing, counted, and
        // the stage falls back to warmup. (Approximate equality from frame 1 on: the servo
        // nudged the ratio off exactly 1.0 after the first fill's above-floor residual, so
        // these frames are interpolated a hair off the grid — that's the servo working.)
        stage.fill(&mut out);
        assert_eq!(diag.snapshot().input_ring_underruns, 7);
        assert_eq!(out[0], 16.0);
        for (n, &got) in out.iter().enumerate().take(9).skip(1) {
            let want = (16 + n) as f32;
            assert!(
                (got - want).abs() < 1e-2,
                "frame {n}: got {got}, want ~{want}"
            );
        }
        assert_eq!(&out[10..], &[0.0; 6], "post-dry frames must be silence");

        // Still dry: warmup again — zeros, and the re-prime silence *keeps counting* (§9
        // know-and-say: a glitching device's delivered silence is fully accounted for; the
        // servo reset the ratio to nominal 1.0, so the counted demand is 16 input frames).
        stage.fill(&mut out);
        assert_eq!(out, [0.0; 16]);
        assert_eq!(diag.snapshot().input_ring_underruns, 7 + 16);

        // Refill past the warmup threshold: flows again from the oldest queued frame, and
        // the counting stops because audio is flowing again.
        push_samples(&mut p, 100..126);
        stage.fill(&mut out);
        assert_eq!(out[0], 100.0);
        assert_eq!(diag.snapshot().input_ring_underruns, 7 + 16);
    }

    #[test]
    fn initial_prefill_stays_uncounted_across_callbacks() {
        // Before the ring has ever flowed, warmup silence is expected — not an underrun —
        // no matter how many callbacks it spans.
        let (mut p, mut stage, diag) = test_stage();
        let mut out = [9.0f32; 16];
        stage.fill(&mut out);
        stage.fill(&mut out);
        push_samples(&mut p, 0..10); // some fill, still below the 26-frame warmup target
        stage.fill(&mut out);
        assert_eq!(out, [0.0; 16]);
        assert_eq!(diag.snapshot().input_ring_underruns, 0);
    }

    #[test]
    fn warmup_completion_trims_the_backlog_to_the_floor_uncounted() {
        let (mut p, mut stage, diag) = test_stage();
        // 60 frames queued before the first pull — early capture (a backend that buffers
        // before `play`), well past the high-water mark (48).
        push_samples(&mut p, 0..60);
        let mut out = [9.0f32; 8];
        stage.fill(&mut out);
        // Demand for 8 frames at ratio 1.0 is 8 + 2 = 10; the warmup-completion trim
        // re-anchors at 10 + floor(8) = 18, dropping the 42 oldest frames *uncounted*
        // (buffered latency nobody heard, not lost audio): output starts at frame 42.
        assert_eq!(diag.snapshot().input_ring_overruns, 0);
        assert_eq!(diag.snapshot().input_ring_underruns, 0);
        for (n, &got) in out.iter().enumerate() {
            assert_eq!(got, (42 + n) as f32, "frame {n}");
        }
        // Residual sits at the floor: 18 kept - 9 consumed (8 + window prime) = 9 ≈ floor 8.
        assert_eq!(stage.consumer.slots(), 9);
    }

    #[test]
    fn midstream_flood_past_high_water_drops_oldest_and_counts() {
        let (mut p, mut stage, diag) = test_stage();
        // Warm normally first: 18 frames covers need (10) + floor (8) for an 8-frame pull.
        push_samples(&mut p, 0..18);
        let mut out = [9.0f32; 8];
        stage.fill(&mut out); // consumes 9, residual 9
        assert_eq!(diag.snapshot().input_ring_overruns, 0);

        // Flood to capacity (9 + 55 = 64 > high water 48): an output stall's backlog. The
        // counted drop-oldest trim must re-anchor fill near the floor immediately instead
        // of leaving the ±0.5% servo to drain ~46 frames of standing latency.
        push_samples(&mut p, 18..73);
        stage.fill(&mut out);
        let s = diag.snapshot();
        assert!(
            s.input_ring_overruns > 0,
            "high-water trim must count dropped frames"
        );
        assert_eq!(s.input_ring_underruns, 0);
        // Post-trim, post-consumption residual is bounded at the floor + the resampler's
        // small slack — not hundreds of frames of dead-zone latency.
        let residual = stage.consumer.slots();
        assert!(
            residual <= 8 + 3,
            "trim must re-anchor at the floor, got residual {residual}"
        );
    }

    #[test]
    fn warmup_gate_is_capped_at_ring_capacity() {
        // A callback demanding more than the ring can hold (need 82 > capacity 64) must
        // still warm once the ring is full — the alternative is permanent, uncounted
        // silence plus unbounded producer-side drops. It flows what exists and counts the
        // shortfall as underruns (§9).
        let (mut p, mut stage, diag) = test_stage();
        let mut out = [9.0f32; 80];
        push_samples(&mut p, 0..64); // ring completely full
        stage.fill(&mut out);
        assert_eq!(out[0], 0.0, "flows from the oldest queued frame");
        assert_eq!(out[63], 63.0, "everything the ring held is delivered");
        assert!(
            diag.snapshot().input_ring_underruns > 0,
            "the physically unsatisfiable remainder must be counted, not silent"
        );
    }

    #[test]
    fn discarding_more_than_available_is_safe_and_whole() {
        let (mut p, mut stage, _diag) = test_stage();
        push_samples(&mut p, 0..4);
        stage.discard_frames(1000); // far beyond fill: clamps, no panic
        assert_eq!(stage.consumer.slots(), 0);
    }

    #[test]
    fn servo_steers_ratio_from_the_residual() {
        // After a fill that leaves the residual above the floor, the next fill consumes
        // faster than nominal (ratio > 1), draining the surplus over time.
        let (mut p, mut stage, _diag) = test_stage();
        // Warmup trims the 40-frame backlog to need (16 + 2 window prime = 18) + floor (8)
        // = 26 uncounted; the resampler then pulls 17 (16 + window prime), so the residual
        // the servo sees is 9 > floor 8 — a strict, deterministic 1-frame surplus.
        push_samples(&mut p, 0..40);
        let mut out = [0.0f32; 16];
        stage.fill(&mut out);
        assert!(
            stage.servo.ratio(stage.nominal_ratio) > 1.0,
            "surplus residual must speed consumption up"
        );
    }
}

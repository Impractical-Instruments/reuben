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
//!   stage then re-enters warmup so a stalled input device re-primes cleanly instead of
//!   counting every subsequent callback forever.
//! - **Ring full → drop oldest**, counted per dropped frame (`input_ring_overruns`): the
//!   consumer trims the *oldest* frames back down to the floor when fill crosses the
//!   high-water mark. (Drop-oldest is necessarily a consumer-side act in an SPSC ring; if the
//!   consumer is stalled outright the producer's only possible move is to drop the *incoming*
//!   frame — the backstop, also counted.)
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
//! (floor + [`RING_HEADROOM_SECS`] of headroom) but capacity is not latency: the servo and
//! the high-water trim keep steady-state fill at the floor.

use std::collections::BTreeMap;
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{SampleFormat, Stream};

use crate::audio::{device_name_matches, AudioError};
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

/// Open the input side (P5/#182): select the device the profile asks for, build the
/// device→logical [`InputMap`], size + allocate the SPSC ring, and build (not yet play) the
/// cpal input stream. Returns the stream (caller keeps it alive and `play()`s it once the
/// output side is running) and the [`InputStage`] the output callback pulls logical input
/// from.
///
/// Only called when the played patch binds input channels (`logical_channels > 0`) — a patch
/// without input pipes never touches an input device. Failures to *open* (no device, no name
/// match, unusable config, non-f32 format, build error) are fatal [`AudioError`]s: the patch
/// explicitly asked for live input, so silently playing without a device would violate
/// "know and say". Channel-count mismatches between the device and the patch are the
/// non-fatal path (§7): warn once, zero-fill, keep playing.
///
/// The input stream runs at the device's **default** config; the profile's
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
    let supported = device
        .default_input_config()
        .map_err(AudioError::InputConfig)?;
    if supported.sample_format() != SampleFormat::F32 {
        return Err(AudioError::UnsupportedInputFormat(
            supported.sample_format(),
        ));
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
        diagnostics,
    );
    Ok((stream, stage))
}

/// Select an input device (ADR-0038 §6, the dual of `select_output_device`): `None` is the
/// host default; `Some(substr)` is the first input device whose name contains `substr`,
/// case-insensitively.
fn select_input_device(
    host: &cpal::Host,
    name_substr: Option<&str>,
) -> Result<cpal::Device, AudioError> {
    match name_substr {
        None => host.default_input_device().ok_or(AudioError::NoInputDevice),
        Some(substr) => {
            let needle = substr.to_lowercase();
            host.input_devices()
                .map_err(AudioError::InputDevicesQuery)?
                .find(|d| {
                    d.name()
                        .map(|n| device_name_matches(&n, &needle))
                        .unwrap_or(false)
                })
                .ok_or_else(|| AudioError::NoMatchingInputDevice(substr.to_string()))
        }
    }
}

/// Build the cpal input stream: the ring's producer side. Per device frame, apply the
/// device→logical map into a preallocated scratch frame, then commit it to the ring as one
/// whole frame ([`push_frame`]) — the consumer never sees a torn frame. Ring full is the
/// producer-side overrun backstop (see the module doc): the incoming frame is dropped and
/// counted.
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
                        diagnostics.record_input_ring_overrun_frames(1);
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
/// commits whole frames, so `slots()` is always a whole number of frames and the per-sample
/// pops below cannot tear.
fn pop_frame(consumer: &mut rtrb::Consumer<f32>, frame: &mut [f32]) -> bool {
    if consumer.slots() < frame.len() {
        return false;
    }
    for s in frame.iter_mut() {
        *s = consumer.pop().unwrap_or(0.0); // unreachable Err: slots() checked above
    }
    true
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
/// - `high_water`: ¾ of capacity — crossing it means the servo lost (consumer stalled or a
///   rate mismatch beyond ±0.5%) and the drop-oldest trim engages.
fn ring_dimensions(block_size: usize, in_rate: f64, engine_rate: f64) -> (usize, usize, usize) {
    let ratio = nominal_ratio(in_rate, engine_rate);
    let floor = ((RING_FLOOR_BLOCKS * block_size) as f64 * ratio)
        .ceil()
        .max(1.0) as usize;
    let headroom_frames = if in_rate.is_finite() && in_rate > 0.0 {
        (in_rate * RING_HEADROOM_SECS).ceil() as usize
    } else {
        0
    };
    let capacity = floor + headroom_frames.max(8 * block_size);
    let high_water = capacity - capacity / 4;
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
/// - identity with a device narrower than the patch's input width → the unsupplied logical
///   channels read silence;
/// - a pair naming a device or logical channel that doesn't exist → dropped;
/// - two device channels feeding the same logical channel → both kept, applied in ascending
///   device order, so the highest device channel wins (named, not accidental — mirrors the
///   output side's collision rule);
/// - a logical channel no pair feeds → reads silence.
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
    let mut pairs = Vec::with_capacity(map.len());
    for (&d, &l) in map {
        if d >= device {
            eprintln!(
                "warning: io-map input.map reads device channel {d}, but the device has \
                 {device} channel(s); dropped"
            );
            continue;
        }
        if l >= logical {
            eprintln!(
                "warning: io-map input.map feeds logical input channel {l}, but the instrument \
                 binds {logical}; dropped"
            );
            continue;
        }
        pairs.push((d, l));
    }
    warn_duplicate_logical_targets(&pairs);
    let mut fed = vec![false; logical];
    for &(_, l) in &pairs {
        fed[l] = true;
    }
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

/// Warn about `input.map` pairs that feed the same logical channel from different device
/// channels (the dual of the output side's duplicate-target warning). `pairs` is in ascending
/// device order and [`InputMap::apply`] applies them in that order, so the *highest* device
/// channel in a collision is the one whose value survives — named explicitly so the behavior
/// isn't an implementation accident.
fn warn_duplicate_logical_targets(pairs: &[(usize, usize)]) {
    let mut by_logical: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for &(d, l) in pairs {
        by_logical.entry(l).or_default().push(d);
    }
    for (l, devices) in by_logical {
        if devices.len() > 1 {
            let winner = *devices.last().expect("just checked len() > 1 above");
            eprintln!(
                "warning: io-map input.map feeds logical input channel {l} from multiple device \
                 channels {devices:?}; device channel {winner} wins (applied last), the rest \
                 are dropped for that logical channel"
            );
        }
    }
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
    /// Drop-oldest trigger, input frames (¾ of capacity).
    high_water: usize,
    /// `false` until the ring has prefilled to (this callback's demand + floor); while
    /// warming, output is zeros and nothing is counted — silence during prefill is expected,
    /// not an underrun.
    warmed: bool,
    diagnostics: Arc<Diagnostics>,
}

impl InputStage {
    fn new(
        consumer: rtrb::Consumer<f32>,
        channels: usize,
        nominal_ratio: f64,
        floor: usize,
        high_water: usize,
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
            warmed: false,
            diagnostics,
        }
    }

    /// Fill `out` (interleaved logical input at the engine rate, `frames * channels` long)
    /// from the ring. Called once per output callback, on the output thread. RT-safe: no
    /// allocation, no locks; the ring ops are wait-free.
    pub(crate) fn fill(&mut self, out: &mut [f32]) {
        let frames = out.len() / self.channels;
        let ratio = self.servo.ratio(self.nominal_ratio);
        // Input frames this callback will pull: the resampled demand, +2 for the window
        // prime (first output frame pulls twice) and the phase remainder.
        let need = (frames as f64 * ratio).ceil() as usize + 2;
        let avail = self.consumer.slots() / self.channels;

        if !self.warmed {
            if avail < need + self.floor {
                // Still prefilling (startup, or re-priming after a dry spell): zeros, not
                // counted — the ADR's underrun is a ring that was flowing and ran dry.
                out.fill(0.0);
                return;
            }
            self.warmed = true;
        }

        // Drop-oldest (ADR-0038 §9): past the high-water mark the servo has lost — discard
        // the *oldest* frames down to exactly what this callback needs plus the floor, count
        // every dropped frame, and let the servo re-center from there.
        if avail > self.high_water {
            let drop_frames = avail.saturating_sub(need + self.floor);
            self.discard_frames(drop_frames);
            self.diagnostics
                .record_input_ring_overrun_frames(drop_frames as u64);
        }

        let (resampler, consumer) = (&mut self.resampler, &mut self.consumer);
        let missed = resampler.process(ratio, out, |frame| pop_frame(consumer, frame));
        if missed > 0 {
            self.diagnostics.record_input_ring_underrun_frames(missed);
            // Ran dry: re-enter warmup for a clean re-prime (fresh window, fresh drift
            // estimate) instead of counting a stalled device forever.
            self.warmed = false;
            self.resampler.reset();
            self.servo.reset();
            return;
        }
        let residual = self.consumer.slots() / self.channels;
        self.servo.update(residual);
    }

    /// Pop and discard `frames` whole frames (the drop-oldest act). The producer only commits
    /// whole frames, so discarding whole frames keeps channel alignment.
    fn discard_frames(&mut self, frames: usize) {
        for _ in 0..frames * self.channels {
            if self.consumer.pop().is_err() {
                break;
            }
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
        // ADR-0038 §7: a mono mic under a stereo-input patch supplies channel 0; channel 1
        // reads silence (warned once at setup).
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
        // frames (~10.7 ms); capacity = floor + 0.5 s; high water at ¾ capacity.
        let (floor, capacity, high_water) = ring_dimensions(256, 48_000.0, 48_000.0);
        assert_eq!(floor, 512);
        assert_eq!(capacity, 512 + 24_000);
        assert_eq!(high_water, capacity - capacity / 4);
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

    // --- InputStage (whole-policy tests over a real ring — no device needed) -------------

    /// A stage over a real rtrb ring with small, legible geometry: mono, ratio 1.0,
    /// floor 8 frames, high water 48, capacity 64.
    fn test_stage() -> (rtrb::Producer<f32>, InputStage, Arc<Diagnostics>) {
        let (producer, consumer) = rtrb::RingBuffer::new(64);
        let diagnostics = Diagnostics::new();
        let stage = InputStage::new(consumer, 1, 1.0, 8, 48, Arc::clone(&diagnostics));
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
    fn dry_ring_counts_underruns_then_reprimes_silently() {
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

        // Still dry: warmup again — zeros, and *no further counting* (a stalled device is
        // one burst of counts, not an avalanche).
        stage.fill(&mut out);
        assert_eq!(out, [0.0; 16]);
        assert_eq!(diag.snapshot().input_ring_underruns, 7);

        // Refill past the warmup threshold: flows again from the oldest queued frame.
        push_samples(&mut p, 100..126);
        stage.fill(&mut out);
        assert_eq!(out[0], 100.0);
        assert_eq!(diag.snapshot().input_ring_underruns, 7);
    }

    #[test]
    fn overfull_ring_drops_oldest_and_counts_overruns() {
        let (mut p, mut stage, diag) = test_stage();
        // 60 frames queued: past the high-water mark (48) on a 64-capacity ring.
        push_samples(&mut p, 0..60);
        let mut out = [9.0f32; 8];
        stage.fill(&mut out);
        // Demand for 8 frames at ratio 1.0 is 8 + 2 = 10; trim drops down to 10 + floor(8):
        // 60 - 18 = 42 oldest frames dropped and counted, so output starts at frame 42.
        assert_eq!(diag.snapshot().input_ring_overruns, 42);
        for (n, &got) in out.iter().enumerate() {
            assert_eq!(got, (42 + n) as f32, "frame {n}");
        }
        assert_eq!(diag.snapshot().input_ring_underruns, 0);
    }

    #[test]
    fn servo_steers_ratio_from_the_residual() {
        // After a fill that leaves the residual above the floor, the next fill consumes
        // faster than nominal (ratio > 1), draining the surplus over time.
        let (mut p, mut stage, _diag) = test_stage();
        push_samples(&mut p, 0..40); // residual after first fill: 40 - 17 = 23 > floor 8
        let mut out = [0.0f32; 16];
        stage.fill(&mut out);
        assert!(
            stage.servo.ratio(stage.nominal_ratio) > 1.0,
            "surplus residual must speed consumption up"
        );
    }
}

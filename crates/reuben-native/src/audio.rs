//! Live audio out via cpal.
//!
//! Opens an output device (the host default, or a [`DeviceProfile`]'s `output.device`
//! substring selection, ADR-0038 §6), builds a [`Coordinator`] + its RT-side [`RenderSlot`]
//! matched to the device sample rate, and renders inside the audio callback by driving the
//! **[`RenderSlot`]** (ADR-0046 §7) rather than an `Engine` directly: the slot owns the live
//! Engine, drains the install mailbox a swap fills, runs ADR-0050's master-gain ramp, and
//! box-transplants survivors — so a swap is **gapless**, with no stream teardown (M2, #323).
//! Incoming decoded OSC ([`OscIn`]) is pulled from an [`std::sync::mpsc::Receiver`] (fed by the
//! OSC/UDP thread) at the top of each callback and typed to a Message against the Plan (ADR-0030).
//!
//! **Streams are fixed at `play` start** (ADR-0046 §6): a swap never reopens a device. The device
//! output map is rebuilt off-thread for the new engine's logical width (against the *retained*
//! device channel count) and shipped across a **parallel render mailbox** ([`swap_pair`]) that the
//! callback drains — the native dual of the core install mailbox, keeping core device-free (§7).
//! The callback installs the new map only when the engine's width has caught up to it, so map and
//! buffer always agree; the transition block is ducked by the ramp.
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
//! When the played instrument binds input channels (ADR-0038 §3), [`start`] also opens the
//! input side (P5/#182, [`crate::input`]): a cpal input stream feeding a lock-free SPSC ring
//! that this module's output callback drains — resampled and drift-compensated into the
//! engine rate — into [`RenderSlot::fill_duplex`]. An instrument without input pipes never
//! touches an input device. A swap to an input-binding engine while no input stream is open
//! **dark-degrades to silence** (the callback feeds `&[]`); the loud warning rode the swap
//! report (ADR-0038 §7/§9), raised on the structure thread at swap time.
//!
//! The returned [`Streams`] must be kept alive for audio to keep playing.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, SupportedBufferSize};
use reuben_core::coordinator::{
    swap_pair, Coordinator, CoordinatorMailbox, RenderMailbox, RenderSide, RenderSlot, SwapInFlight,
};
use reuben_core::format::LoadWarning;
use reuben_core::message::Message;
use reuben_core::{AudioConfig, Diag};

use crate::diagnostics::Diagnostics;
use crate::osc::OscIn;
use crate::profile::DeviceProfile;
use crate::structure::{dark_degrade_warning, RenderConfigPublisher};

/// How often the periodic diagnostics logger wakes to check the counters (ADR-0038 §9). It only
/// emits a line when something changed, so a healthy run stays quiet at this cadence regardless.
const DIAGNOSTICS_LOG_INTERVAL: Duration = Duration::from_secs(5);

/// How long [`NativeRenderConfig::publish`] polls to install a swap's output map before giving up
/// (B1). The map mailbox is one-in-flight (ADR-0046 §2): install is refused until the *previous*
/// swap's displaced map has come home, which the live render callback posts within ~one master-gain
/// ramp (ADR-0050). This bound only bites when audio has genuinely stopped — and even then
/// [`apply_output_map`]'s total read keeps a stale-width map safe. Generous, matching the structure
/// channel's engine-reclaim bound.
const RENDER_CONFIG_INSTALL_TIMEOUT: Duration = Duration::from_millis(500);

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
    /// The played instrument binds input channels but there is no default input device
    /// (P5/#182). Fatal by design — a deliberate, recorded ADR-0038 §7 carve-out: the
    /// instrument explicitly asked for live input, so playing silently without a device
    /// would violate §9's "know and say" (§7's dark-degrade covers *channel* mismatches on
    /// a device that did open, not the absence of any device).
    NoInputDevice,
    /// No input device's name contains the profile's `input.device` substring.
    NoMatchingInputDevice(String),
    /// Enumerating input devices failed.
    InputDevicesQuery(cpal::DevicesError),
    /// The input device reported an unusable default config.
    InputConfig(cpal::DefaultStreamConfigError),
    /// Querying the input device's supported configs failed (only reached when its default
    /// format isn't f32 and `crate::input` negotiates for one).
    InputSupportedConfigs(cpal::SupportedStreamConfigsError),
    /// The input device has no f32-capable config at all — its default format is carried
    /// here. Only returned after `supported_input_configs` was searched (the default merely
    /// being non-f32 negotiates instead), so this is a statement about the hardware.
    UnsupportedInputFormat(SampleFormat),
    /// The input device's usable config reports zero channels; opening it would arm a
    /// guaranteed panic in the RT input callback.
    NoInputChannels,
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
            AudioError::InputSupportedConfigs(e) => {
                write!(f, "query supported input configs: {e}")
            }
            AudioError::UnsupportedInputFormat(fmt) => {
                write!(
                    f,
                    "input device has no f32-capable config (default sample format {fmt:?}; \
                     only f32 for now)"
                )
            }
            AudioError::NoInputChannels => {
                write!(f, "input device reports zero input channels")
            }
            AudioError::BuildInput(e) => write!(f, "build input stream: {e}"),
            AudioError::PlayInput(e) => write!(f, "play input stream: {e}"),
        }
    }
}

impl std::error::Error for AudioError {}

/// The live cpal streams [`start`] returns — keep the whole struct alive for audio to keep
/// flowing. `input` is `Some` only when the played instrument binds input channels
/// (ADR-0038 §3): an instrument without input pipes never touches an input device (P5/#182).
pub struct Streams {
    pub output: Stream,
    pub input: Option<Stream>,
}

/// The live audio session [`start`] returns. Keep [`streams`](LiveAudio::streams) alive for audio
/// to keep flowing; hand [`coordinator`](LiveAudio::coordinator) to the structure channel (ADR-0046
/// §7, the single writer of graph structure) and [`render_config`](LiveAudio::render_config) to it
/// as the device seam a swap publishes its output map through. [`warnings`](LiveAudio::warnings)
/// are the initial instrument's non-fatal load warnings (ADR-0016), for the caller to surface.
pub struct LiveAudio {
    /// The live cpal stream(s) — dropping this stops audio (streams are fixed for the session,
    /// ADR-0046 §6; a swap never reopens them).
    pub streams: Streams,
    /// The shared xrun/ring counter surface both callbacks feed (ADR-0038 §9).
    pub diagnostics: Arc<Diagnostics>,
    /// The passive Coordinator (ADR-0046 §7): the structure channel owns it and drives every swap.
    pub coordinator: Coordinator,
    /// The native device seam the structure server publishes each swap's output map + dark-degrade
    /// warning through ([`RenderConfigPublisher`], ADR-0046 §6 / ADR-0038 §7).
    pub render_config: Arc<dyn RenderConfigPublisher>,
    /// The initial instrument's non-fatal load warnings (ADR-0016).
    pub warnings: Vec<LoadWarning>,
}

/// Start live playback on an output device, per `profile` (ADR-0038 §6).
///
/// `block_size` is the core render block size; `build` constructs the [`Coordinator`] + its RT
/// [`RenderSide`] once the device sample rate is known (so the Plan's tuning matches the hardware)
/// — typically a call to [`Coordinator::install_initial`]. The RT-side [`RenderSlot`] built from
/// that `RenderSide` is what the callback drives (ADR-0046 §7), draining the install mailbox a swap
/// fills; the Coordinator is returned for the structure channel. `osc_out` is the optional OSC-out
/// sink (ADR-0026): when `Some`, the callback forwards each outbound Message to it (a sender thread
/// encodes + UDP-sends, off the audio thread); when `None`, outbound is drained and dropped, with
/// one warning the first time a rig sends. `profile` selects the devices, negotiates
/// sample-rate/buffer-size preferences, and overrides the channel maps — pass
/// [`DeviceProfile::default`] for today's behavior (default devices, identity maps).
///
/// **Streams are fixed for the session** (ADR-0046 §6): a swap never reopens a device. The device
/// output map is rebuilt off-thread for each swapped-in engine (against the *retained* device
/// channel count) and shipped across a parallel render mailbox the callback drains — see
/// [`NativeRenderConfig`], returned as [`LiveAudio::render_config`].
///
/// When the built engine binds input channels, the input side opens too (P5/#182,
/// [`crate::input`]): a cpal input stream on the profile's `input.device` (default input device
/// otherwise) feeds a lock-free ring; each output callback pulls that ring through the
/// resampling/drift-compensating [`crate::input::InputStage`] and hands the result to
/// [`RenderSlot::fill_duplex`]. A swap to an input-binding engine while no input stream is open
/// **dark-degrades to silence** (the callback feeds `&[]`, ADR-0038 §7).
pub fn start<F>(
    osc_rx: Receiver<OscIn>,
    block_size: usize,
    osc_out: Option<Sender<Message>>,
    profile: &DeviceProfile,
    build: F,
) -> Result<LiveAudio, AudioError>
where
    F: FnOnce(AudioConfig) -> (Coordinator, RenderSide, Vec<LoadWarning>),
{
    let host = cpal::default_host();
    let device = select_output_device(&host, profile.output.device.as_deref())?;

    let (sample_format, config) =
        negotiate_output_config(&device, profile.sample_rate, profile.buffer_size)?;
    if sample_format != SampleFormat::F32 {
        return Err(AudioError::UnsupportedFormat(sample_format));
    }

    // The device channel count, retained for the whole session (ADR-0046 §6): a swap rebuilds the
    // logical→device output map against *this* count, never reopening the stream.
    let channels = config.channels as usize;
    let sample_rate = config.sample_rate.0 as f32;

    // Build the Coordinator + RT RenderSide at the device rate. The RenderSlot drives the Engine in
    // the callback (ADR-0046 §7); the Coordinator goes to the structure channel.
    let (coordinator, render_side, warnings) = build(AudioConfig::new(sample_rate, block_size));
    let mut slot = RenderSlot::new(render_side);
    let logical = slot.channels();
    let in_channels = slot.input_channels();

    // The initial device output map, plus the parallel render mailbox a swap ships the *next* map
    // across (ADR-0046 §6). The callback owns the render end + the active map; the structure server
    // owns the coordinator end (through the returned NativeRenderConfig).
    let initial_map = build_output_map(&profile.output.map, logical, channels);
    let (map_coord, map_render) = swap_pair::<RenderConfig>();
    let mut output_map = OutputMapSlot::new(
        map_render,
        Box::new(RenderConfig {
            map: initial_map,
            logical,
        }),
    );

    // Scratch for one callback's worth of interleaved logical samples; grows to the largest
    // callback (audio-thread allocation only while warming up — and, once per swap that *widens*
    // the logical output, on that swap's ramp-ducked transition block — never in steady state).
    let mut buf: Vec<f32> = Vec::new();
    // The input-side dual of `buf`. Same warmup-only growth policy; empty for a no-input instrument.
    let mut in_buf: Vec<f32> = Vec::new();
    // Warn at most once if a rig sends OSC out with no target configured (ADR-0026).
    let mut warned_no_target = false;

    let diagnostics = Diagnostics::new();
    let diag_for_callback = Arc::clone(&diagnostics);
    crate::diagnostics::spawn_periodic_logger(Arc::clone(&diagnostics), DIAGNOSTICS_LOG_INTERVAL);

    // Input opens ONLY when the *initial* played instrument binds input channels (ADR-0038 §3/P5);
    // its width is fixed for the session. A later swap that binds input while no matching stream is
    // open dark-degrades (the callback feeds `&[]`); the warning rode the swap report.
    let opened_input_channels = in_channels;
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
                // acceptable, deliberate exception to "no syscalls in the callback".
                let callback_start = Instant::now();

                while let Ok(m) = osc_rx.try_recv() {
                    // Convert flat OSC -> typed Message at the slot's Engine, where the Plan (and so
                    // each dest port's Arg type) is known (ADR-0030).
                    slot.queue_osc(&m.address, &m.args);
                }

                // The current engine's logical output width (a swap may have just changed it), read
                // once so buffer sizing and the output map agree. `sync` installs the pending map
                // only when its width matches this — RT-safe (one atomic drain + pointer moves; the
                // displaced map is posted back for off-thread free, never dropped here).
                let logical = slot.channels();
                output_map.sync(logical);
                debug_assert_eq!(
                    output_map.active_logical(),
                    logical,
                    "output map width must track the live engine's logical width"
                );

                let in_channels = slot.input_channels();
                let frames = data.len() / channels;
                if buf.len() < frames * logical {
                    buf.resize(frames * logical, 0.0);
                }

                // Input: feed the ring only when a stream is open AND its fixed width matches the
                // engine's; otherwise dark-degrade to silence (ADR-0038 §7 — a swap changed the
                // input geometry, which needs a `play` restart, ADR-0046 §6). `fill_duplex(&[])`
                // stages honest device silence into the engine's bound input pipes.
                let input: &[f32] = match &mut input_stage {
                    Some(stage) if in_channels > 0 && in_channels == opened_input_channels => {
                        if in_buf.len() < frames * in_channels {
                            in_buf.resize(frames * in_channels, 0.0);
                        }
                        stage.fill(&mut in_buf[..frames * in_channels]);
                        &in_buf[..frames * in_channels]
                    }
                    _ => &[],
                };
                slot.fill_duplex(input, &mut buf[..frames * logical]);

                // Forward this callback's outbound Messages (ADR-0026). The sender thread does the
                // UDP I/O; the audio thread only hands off. No target -> drain and drop, warning
                // once so a misconfigured feedback rig isn't silently dead.
                for m in slot.drain_outbound() {
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
                    apply_output_map(output_map.map(), src, dst);
                }

                // The budget is this callback's own frame count over the sample rate (cpal is free
                // to ask for a different frame count than the core block), generalizing ADR-0038
                // §9's `block_size / sample_rate` to the actual `frames` this callback rendered.
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

    let render_config = Arc::new(NativeRenderConfig {
        mailbox: Mutex::new(map_coord),
        device_channels: channels,
        output_map: profile.output.map.clone(),
        opened_input_channels,
    });
    Ok(LiveAudio {
        streams: Streams {
            output: stream,
            input: input_stream,
        },
        diagnostics,
        coordinator,
        render_config,
        warnings,
    })
}

/// What crosses the parallel render mailbox (ADR-0046 §6): the freshly-validated device output map
/// for a swapped-in engine, tagged with the engine's **logical output width** so the callback
/// installs it only once the live engine has caught up to that width (map and buffer widths then
/// always agree; the transition block is ducked by the master-gain ramp, ADR-0050). This is the
/// native dual of core's [`InstallBundle`](reuben_core::coordinator::InstallBundle); it stays in
/// reuben-native so core never learns the device's channel count (ADR-0046 §7).
struct RenderConfig {
    /// The logical→device output map, validated against the retained device channel count.
    map: OutputMap,
    /// The logical output width `map` was built for; the callback promotes `map` to active when the
    /// live engine's [`RenderSlot::channels`] equals this.
    logical: usize,
}

/// The RT-side output-map install slot — the native dual of core's [`RenderSlot`] for the *device*
/// output map (ADR-0046 §6). Holds the active map plus a pending one drained from the render
/// mailbox, and promotes the pending map to active only when the engine's logical width has caught
/// up to it (so the applied map always matches the buffer width for the block). All pointer moves +
/// at most one atomic drain and one atomic post: **no alloc, free, lock, or drop on the render
/// thread**. The displaced map is posted back through the mailbox for off-thread free (never
/// dropped here); a post the retire slot somehow refuses is stashed and re-posted next callback,
/// exactly as core's `RenderSlot` handles its stranded retiree.
struct OutputMapSlot {
    render: RenderMailbox<RenderConfig>,
    active: Box<RenderConfig>,
    pending: Option<Box<RenderConfig>>,
    stranded: Option<Box<RenderConfig>>,
}

impl OutputMapSlot {
    fn new(render: RenderMailbox<RenderConfig>, active: Box<RenderConfig>) -> Self {
        Self {
            render,
            active,
            pending: None,
            stranded: None,
        }
    }

    /// Sync the active map to `engine_logical`, the live engine's current output width. RT-safe.
    fn sync(&mut self, engine_logical: usize) {
        // Re-post a stranded retiree if a post was ever (impossibly, under one-in-flight) refused:
        // never drop the box on the render thread.
        if let Some(retiree) = self.stranded.take() {
            if let Err(returned) = self.render.post_retiree(retiree) {
                self.stranded = Some(returned);
            }
        }
        // Pick up a newly published map (one atomic drain, a move into the Option — no alloc).
        if self.pending.is_none() {
            if let Some(cfg) = self.render.take_install() {
                self.pending = Some(cfg);
            }
        }
        // Promote when the engine's width has caught up to the pending map's width, so the map
        // matches the buffer this callback renders. A width-preserving swap promotes immediately
        // (the widths already match); a widening swap waits for the engine to install first — its
        // ducked transition block keeps the old (matching-width) map, so nothing ever indexes out
        // of range. Total by construction (no unwrap on the render thread): take the box, and put
        // it back untouched if the engine has not caught up yet.
        if let Some(pending) = self.pending.take() {
            if pending.logical == engine_logical {
                let old = std::mem::replace(&mut self.active, pending);
                if let Err(returned) = self.render.post_retiree(old) {
                    self.stranded = Some(returned);
                }
            } else {
                self.pending = Some(pending);
            }
        }
    }

    /// The active device output map to apply this callback.
    #[inline]
    fn map(&self) -> &OutputMap {
        &self.active.map
    }

    /// The logical width the active map was built for — must equal the live engine's width every
    /// callback (a debug-asserted invariant; see [`sync`](Self::sync)).
    #[inline]
    fn active_logical(&self) -> usize {
        self.active.logical
    }
}

/// The production [`RenderConfigPublisher`] (ADR-0046 §6): the native device seam of the M2 swap.
/// After [`Coordinator::swap_document`] commits, [`publish`](RenderConfigPublisher::publish)
/// rebuilds the device output map off-thread for the new engine's logical width against the
/// *retained* `device_channels`, ships it across the render mailbox for the callback to install
/// (synchronized to the engine, [`OutputMapSlot`]), and returns the input dark-degrade warning
/// (ADR-0038 §7/§9). The structure server calls it under the Coordinator lock, one swap at a time.
pub struct NativeRenderConfig {
    /// The coordinator end of the parallel render mailbox; `Mutex` only to take `&mut` for the
    /// (uncontended, one-swap-at-a-time) install/reclaim — never touched by the render thread.
    mailbox: Mutex<CoordinatorMailbox<RenderConfig>>,
    /// The retained device channel count (ADR-0046 §6) — a swap never changes it.
    device_channels: usize,
    /// The profile's `output.map` (fixed for the session): logical→device pairs, or empty for the
    /// implicit broadcast/downmix/zero-fill policy.
    output_map: BTreeMap<usize, usize>,
    /// Logical input channels the (fixed) input stream provides; `0` for an output-only stream.
    opened_input_channels: usize,
}

impl RenderConfigPublisher for NativeRenderConfig {
    fn publish(&self, logical: usize, input_channels: usize) -> Vec<Diag> {
        // Build the new engine's device output map off-thread, validated against the retained
        // device channel count (ADR-0046 §6).
        let map = build_output_map(&self.output_map, logical, self.device_channels);
        let mut cfg = Box::new(RenderConfig { map, logical });
        {
            let mut mailbox = self.mailbox.lock().expect("render config mailbox poisoned");
            // Ship the new map, and **never drop it** (B1). A dropped map desyncs the two mailboxes:
            // the engine advances to the new width while the callback's active map stays at the old
            // one, and `apply_output_map` is then handed a stale-width map. The map mailbox is
            // one-in-flight (ADR-0046 §2), so `install` is refused until the *previous* swap's
            // displaced map has come home — which the live render callback posts at that swap's
            // promote (within ~one ramp). This call runs *before* the structure thread's engine
            // reclaim that proves the callback is consuming, so poll reclaim+install to a bounded
            // deadline rather than dropping: the retiree arrives promptly, and the map is guaranteed
            // installed before the swap returns, so the callback promotes it the moment the engine
            // reaches the new width (no desync window). A timeout only bites if audio has genuinely
            // stopped, and even then `apply_output_map`'s total read keeps the stale map safe.
            let deadline = Instant::now() + RENDER_CONFIG_INSTALL_TIMEOUT;
            loop {
                let _ = mailbox.try_reclaim();
                match mailbox.install(cfg) {
                    Ok(()) => break,
                    Err(SwapInFlight { rejected }) => {
                        cfg = rejected;
                        if Instant::now() >= deadline {
                            // Audio isn't consuming maps; drop this update (off the audio thread).
                            // The old map keeps working and `apply_output_map`'s total read is safe.
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
            }
        }
        // The input dark-degrade decision is native geometry too (ADR-0038 §7/§9).
        dark_degrade_warning(input_channels, self.opened_input_channels)
    }
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
/// `Some(substr)` is the first device whose name contains `substr`, case-insensitively (the
/// [`find_named_device`] kernel shared with the input side).
fn select_output_device(
    host: &cpal::Host,
    name_substr: Option<&str>,
) -> Result<cpal::Device, AudioError> {
    match name_substr {
        None => host.default_output_device().ok_or(AudioError::NoDevice),
        Some(substr) => {
            let devices = host.output_devices().map_err(AudioError::DevicesQuery)?;
            find_named_device(devices, substr)
                .ok_or_else(|| AudioError::NoMatchingDevice(substr.to_string()))
        }
    }
}

/// The name-substring device search behind `output.device` and (via [`crate::input`])
/// `input.device` selection — one kernel, so the two sides' matching rules can't drift.
pub(crate) fn find_named_device(
    mut devices: impl Iterator<Item = cpal::Device>,
    substr: &str,
) -> Option<cpal::Device> {
    let needle = substr.to_lowercase();
    devices.find(|d| {
        d.name()
            .map(|n| device_name_matches(&n, &needle))
            .unwrap_or(false)
    })
}

/// The case-insensitive substring match behind [`find_named_device`], pulled out so it has a
/// unit test that doesn't need a real [`cpal::Host`] (review finding #6) — `needle` is
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
/// counts once, here (the [`validate_map_pairs`] kernel shared with the input side): a pair
/// naming a channel that doesn't exist on either side is a reality mismatch (ADR-0038 §7) —
/// warned about now and dropped, not fatal. Two *different* logical channels naming the
/// *same* device channel are also a reality mismatch (review finding #1): both pairs are kept
/// (so the mapping is still fully described), but colliding targets are warned about once,
/// here ([`for_each_duplicate_target`]), since [`apply_output_map`] applies pairs in
/// ascending-logical order and the higher logical channel silently wins otherwise.
fn build_output_map(map: &BTreeMap<usize, usize>, logical: usize, device: usize) -> OutputMap {
    if map.is_empty() {
        return OutputMap::Identity;
    }
    let (pairs, mapped) = validate_map_pairs(
        map,
        logical,
        device,
        |l| {
            eprintln!(
                "warning: io-map output.map logical channel {l} does not exist (instrument has \
                 {logical} logical channel(s)); dropped"
            );
        },
        |d| {
            eprintln!(
                "warning: io-map output.map targets device channel {d}, but the device has \
                 {device} channel(s); dropped"
            );
        },
    );
    for_each_duplicate_target(&pairs, |d, logicals, winner| {
        eprintln!(
            "warning: io-map output.map targets device channel {d} from multiple logical \
             channels {logicals:?}; logical channel {winner} wins (applied last), the rest \
             are dropped for that device channel"
        );
    });
    OutputMap::Explicit { pairs, mapped }
}

/// The validate-and-mask kernel behind [`build_output_map`] and its input dual
/// (`crate::input`'s `build_input_map`) — one implementation, so the two sides' §7
/// warn+degrade rules can't drift apart. Walks a profile map's `(from, to)` pairs in
/// ascending `from` order (the `BTreeMap`'s iteration order — also the application order both
/// sides document), drops each out-of-range pair through the side's own warning closure, and
/// returns the surviving pairs plus the `to`-side mask of channels at least one pair feeds
/// (the rest degrade to silence/zero-fill).
pub(crate) fn validate_map_pairs(
    map: &BTreeMap<usize, usize>,
    from_bound: usize,
    to_bound: usize,
    mut warn_from_out_of_range: impl FnMut(usize),
    mut warn_to_out_of_range: impl FnMut(usize),
) -> (Vec<(usize, usize)>, Vec<bool>) {
    let mut pairs = Vec::with_capacity(map.len());
    for (&from, &to) in map {
        if from >= from_bound {
            warn_from_out_of_range(from);
            continue;
        }
        if to >= to_bound {
            warn_to_out_of_range(to);
            continue;
        }
        pairs.push((from, to));
    }
    let mut mask = vec![false; to_bound];
    for &(_, to) in &pairs {
        mask[to] = true;
    }
    (pairs, mask)
}

/// The duplicate-target collision rule shared by `output.map` (review finding #1) and its
/// input dual: group validated `(source, target)` pairs by target and hand every collision
/// (two or more sources feeding one target) to `warn` as
/// `(target, sources_in_application_order, winner)`. Both sides apply pairs in ascending
/// source order, so the *highest* colliding source is the one whose value survives — named
/// explicitly (once, here) so the behavior isn't just an implementation accident.
pub(crate) fn for_each_duplicate_target(
    pairs: &[(usize, usize)],
    mut warn: impl FnMut(usize, &[usize], usize),
) {
    let mut by_target: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for &(source, target) in pairs {
        by_target.entry(target).or_default().push(source);
    }
    for (target, sources) in by_target {
        if sources.len() > 1 {
            let winner = *sources.last().expect("just checked len() > 1 above");
            warn(target, &sources, winner);
        }
    }
}

/// Apply the active output mapping to one frame. `Identity` defers to [`map_frame`]'s policy;
/// `Explicit` zeros every device channel the map doesn't target (ADR-0038 §7's degrade-to-silence)
/// and then copies each validated `(logical, device)` pair. Allocation-free: `pairs`/`mapped` are
/// built once at stream setup, never in the render callback.
///
/// **Total by construction (RT-safety).** The Explicit read is `logical_frame.get(l)`, not
/// `logical_frame[l]`, so a momentary width disagreement between the active map and the buffer
/// (`l >= logical_frame.len()`) degrades to a benign zero-fed device channel instead of an
/// out-of-bounds **panic on the render thread** — the same totality the `Identity` arm's
/// `map_frame` already has. The map/buffer widths are kept in lockstep by [`OutputMapSlot`] +
/// [`NativeRenderConfig::publish`] (map install can never be dropped while the engine advances), so
/// this fallback is defense-in-depth, only ever reachable in the ramp-ducked transition of a
/// width-changing swap.
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
                device_frame[d] = logical_frame.get(l).copied().unwrap_or(0.0);
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
    fn more_logical_than_device_channels_drops_the_extras() {
        // `map_frame`'s fourth documented case: more logical channels than device channels
        // (device > 1) copies the leading channels and drops the rest — never fatal
        // (ADR-0038 §7). Not hypothetical: stereo-sub binds three logical channels
        // (main_l/main_r/sub), so a plain stereo interface with no profile lands here.
        // The 9.0 sentinel proves both device slots were overwritten, not skipped.
        let mut dev = [9.0f32; 2];
        map_frame(&[0.1, 0.2, 0.3], &mut dev);
        assert_eq!(dev, [0.1, 0.2]);
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
        // existing instruments hit (stereo, mono downmix, extra device channels, dropped extras).
        let cases: &[(&[f32], usize)] = &[
            (&[0.25, -0.5], 2),
            (&[0.2, 0.4], 1),
            (&[0.123_456_79, 0.123_456_79], 1),
            (&[0.1, 0.2], 4),
            (&[0.1, 0.2, 0.3], 2),
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

    #[test]
    fn output_map_slot_waits_for_the_engine_to_widen_before_promoting() {
        // The RT-safe sync (ADR-0046 §6): a swap that *widens* the logical output ships a wider map,
        // but the callback keeps the old (matching-width) map until the engine actually installs the
        // wider Plan — so the applied map never indexes past the buffer. The displaced map is posted
        // back for off-thread free, never dropped on the render thread.
        let (mut coord, render) = swap_pair::<RenderConfig>();
        let mut slot = OutputMapSlot::new(
            render,
            Box::new(RenderConfig {
                map: OutputMap::Identity,
                logical: 2,
            }),
        );
        assert_eq!(slot.active_logical(), 2);

        // Publish a 3-wide map. While the live engine is still 2-wide, it stays pending.
        coord
            .install(Box::new(RenderConfig {
                map: OutputMap::Identity,
                logical: 3,
            }))
            .expect("install the pending map");
        slot.sync(2);
        assert_eq!(
            slot.active_logical(),
            2,
            "a widening map waits for the engine to install first"
        );
        assert!(
            coord.try_reclaim().is_none(),
            "nothing posted back yet — no map has been displaced"
        );

        // The engine installs the wider Plan: now the map promotes, and the old one comes home.
        slot.sync(3);
        assert_eq!(
            slot.active_logical(),
            3,
            "promoted once the engine caught up to the map's width"
        );
        let retiree = coord
            .try_reclaim()
            .expect("the displaced map is posted back for off-thread free");
        assert_eq!(retiree.logical, 2, "the retiree is the old (2-wide) map");
    }

    #[test]
    fn output_map_slot_promotes_a_width_preserving_map_immediately() {
        // The common case: a swap that keeps the logical width (e.g. every stereo→stereo edit)
        // promotes its map on the first sync — the widths already agree — and posts the old map back
        // for off-thread free. No alloc/free/drop happens on the render thread in either path.
        let (mut coord, render) = swap_pair::<RenderConfig>();
        let mut slot = OutputMapSlot::new(
            render,
            Box::new(RenderConfig {
                map: OutputMap::Identity,
                logical: 2,
            }),
        );
        coord
            .install(Box::new(RenderConfig {
                map: OutputMap::Identity,
                logical: 2,
            }))
            .expect("install");
        slot.sync(2);
        assert_eq!(slot.active_logical(), 2);
        assert!(
            coord.try_reclaim().is_some(),
            "the same-width map promoted immediately and the old one was posted for off-thread free"
        );
    }

    /// An instrument whose logical output width is `max_channel + 1` (ADR-0038 §3, floor 2): one
    /// oscillator broadcast to logical channels 0 and `max_channel`. `width_doc(1)` is 2 wide,
    /// `width_doc(3)` is 4 wide — distinct docs the Coordinator swaps between.
    fn width_doc(max_channel: usize) -> String {
        format!(
            r#"{{ "format_version": 3, "instrument": "w",
                 "interface": {{ "outputs": {{
                    "a": {{ "from": "/osc.audio", "channel": 0 }},
                    "b": {{ "from": "/osc.audio", "channel": {max_channel} }} }} }},
                 "nodes": [ {{ "type": "oscillator", "address": "/osc" }} ] }}"#
        )
    }

    #[test]
    fn back_to_back_width_changing_swaps_keep_the_output_map_in_lockstep() {
        // Regression for B1: a real `NativeRenderConfig` shipping maps across the render mailbox to
        // an `OutputMapSlot`-driven fake callback (the real callback structure, not the map-less
        // `RenderSlot::fill` fake), fired through two consecutive *width-changing* swaps — a widen
        // (2→4) then a narrow (4→2) — with a device `output.map` referencing logical channel 3.
        //
        // The narrowing swap's map drops that channel-3 pair; if `publish` were allowed to drop the
        // map while the previous one is still in flight (the pre-fix bug), the callback's active map
        // would stay 4-wide while the engine/buffer narrowed to 2, and `apply_output_map` would read
        // `logical_frame[3]` on a 2-wide frame — an out-of-bounds panic on the render thread (or,
        // with the total read, a stale-width misroute). The fake callback flags any block where the
        // active map width disagrees with the live engine width. With the fix (publish never drops;
        // total `apply_output_map`) there is no desync and no panic.
        use reuben_core::resources::MemoryResolver;
        use reuben_core::Registry;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let device_channels = 6usize;
        let mut profile_map = BTreeMap::new();
        profile_map.insert(0usize, 0usize);
        profile_map.insert(3usize, 3usize); // valid only when the logical width exceeds 3

        let (mut coordinator, side, _w) = Coordinator::install_initial(
            &width_doc(1),
            Registry::builtin(),
            Box::new(MemoryResolver::new()),
            AudioConfig::new(48_000.0, 128),
        )
        .expect("initial install");
        let init_logical = coordinator.installed_channels();
        assert_eq!(
            init_logical, 2,
            "the initial rig is 2 logical channels wide"
        );

        let (map_coord, map_render) = swap_pair::<RenderConfig>();
        let publisher = NativeRenderConfig {
            mailbox: Mutex::new(map_coord),
            device_channels,
            output_map: profile_map.clone(),
            opened_input_channels: 0,
        };
        let initial_map = build_output_map(&profile_map, init_logical, device_channels);
        let map_slot = OutputMapSlot::new(
            map_render,
            Box::new(RenderConfig {
                map: initial_map,
                logical: init_logical,
            }),
        );

        // The fake audio callback: the real per-block structure (sync → fill → apply), flagging a
        // desync whenever the active map width ever disagrees with the live engine width.
        let stop = Arc::new(AtomicBool::new(false));
        let desync = Arc::new(AtomicBool::new(false));
        let blocks = Arc::new(AtomicUsize::new(0));
        let cb = {
            let (stop, desync, blocks) =
                (Arc::clone(&stop), Arc::clone(&desync), Arc::clone(&blocks));
            std::thread::spawn(move || {
                let mut slot = RenderSlot::new(side);
                let mut map_slot = map_slot;
                let mut device = vec![0.0f32; 64 * device_channels];
                while !stop.load(Ordering::SeqCst) {
                    let logical = slot.channels();
                    map_slot.sync(logical);
                    if map_slot.active_logical() != logical {
                        desync.store(true, Ordering::SeqCst);
                    }
                    let mut buf = vec![0.0f32; 64 * logical];
                    slot.fill(&mut buf);
                    for (frame, dst) in device.chunks_mut(device_channels).enumerate() {
                        let src = &buf[frame * logical..frame * logical + logical];
                        // Would OOB-panic on the render thread without the total read (part a).
                        apply_output_map(map_slot.map(), src, dst);
                    }
                    blocks.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(2)); // widen the race window
                }
            })
        };

        // `handle_swap`'s order: swap_document → publish (never drops the map) → reclaim engine.
        let commit = |coordinator: &mut Coordinator, json: &str, want_width: usize| {
            let report = coordinator.swap_document(json, None);
            assert!(report.report.ok, "swap should install: {:?}", report.report);
            let logical = coordinator.installed_channels();
            assert_eq!(
                logical, want_width,
                "swap changed the logical width as intended"
            );
            let _ = publisher.publish(logical, coordinator.installed_input_channels());
            let deadline = Instant::now() + Duration::from_secs(2);
            let _ = coordinator.reclaim(|| {
                std::thread::sleep(Duration::from_millis(1));
                Instant::now() >= deadline
            });
        };

        commit(&mut coordinator, &width_doc(3), 4); // widen 2 → 4
        commit(&mut coordinator, &width_doc(1), 2); // narrow 4 → 2 (drops the channel-3 map pair)

        // Let the callback settle onto the final width, then stop and join (off-thread drop).
        std::thread::sleep(Duration::from_millis(60));
        stop.store(true, Ordering::SeqCst);
        cb.join().expect("callback thread joined");

        assert!(
            blocks.load(Ordering::SeqCst) > 0,
            "the callback actually rendered"
        );
        assert!(
            !desync.load(Ordering::SeqCst),
            "the device output map stayed in lockstep with the engine width across two \
             width-changing swaps (B1) — no dropped map, no stale-width apply"
        );
    }
}

//! The engine entry point: everything `reuben play` starts, as one call another binary can make.
//!
//! [`start`] loads the instrument, opens the audio device (and the input device when the
//! instrument binds input channels) per the [`DeviceProfile`], binds the OSC-in UDP listener and
//! the optional OSC-out sender, serves the engine channel every client dials, and
//! (unless [`EngineConfig::follow_files`] is off) follows outside edits to the instrument's files.
//! The [`RunningEngine`] it returns is the whole session: it reaches the engine in-process through
//! the window's own [`Channel`], and dropping it stops every thread and frees every port.
//!
//! In-process and over the socket are the same channel, not two paths: [`RunningEngine::channel`]
//! is a [`Channel`] over [`InProcess`], which dispatches into the very [`ChannelState`] the
//! [`ChannelServer`] serves. A caller drives it with the window's engine verbs
//! ([`open_instrument`](reuben_api::engine::open_instrument),
//! [`send_live_controls`](reuben_api::engine::send_live_controls),
//! [`get_engine_status`](reuben_api::engine::get_engine_status), …), and an open made in-process is
//! the document the sidecar reads next, because there is one Coordinator behind both.

use std::fmt;
use std::io;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use reuben_api::authoring::{DocumentHistory, Resources};
use reuben_api::engine::{
    Channel, ChannelState, FromDocumentError, InProcess, Installed, LoadWarning, Token,
    DEFAULT_ENGINE_ADDR, SILENCE,
};
use reuben_api::render::{self, AudioConfig, Message, RenderSide, RenderSlot};
use reuben_api::staged::Staged;
use reuben_api::FsResolver;

use crate::audio::{self, drain_control_each, AudioError};
use crate::control::{self, ControlDrain, ControlProducer};
use crate::diagnostics::{self, Diagnostics, PeriodicLogger, Snapshot};
use crate::engine_channel::{
    ChannelServer, HeadlessRenderConfig, NativeHost, RenderConfigPublisher, SwapHold,
};
use crate::meters::Meters;
use crate::midi::devices::MidiInputs;
use crate::midi::router::{Router, SharedRouter};
use crate::midi::watch::RouteWatch;
use crate::osc::{self, ControlBatch, DEFAULT_OSC_PORT};
use crate::profile::DeviceProfile;
use crate::rigs::DEFAULT_JSON;
use crate::tree_watch::{self, TreeWatch};

/// The core render block size `reuben play` runs at.
pub const DEFAULT_BLOCK_SIZE: usize = 256;

/// The sample rate a headless engine renders at.
pub const HEADLESS_SAMPLE_RATE: f32 = 48_000.0;

/// The block size a headless engine renders at.
pub const HEADLESS_BLOCK_SIZE: usize = 128;

/// How far behind its deadline the real-time headless driver may fall before it gives up catching
/// up and restarts its clock: a stall (a suspended process, a starved thread) is not paid back as a
/// burst of blocks rendered back to back.
const HEADLESS_MAX_LAG_BLOCKS: u32 = 8;

/// How often the periodic diagnostics logger wakes to check the counters. It only emits a line when
/// something changed, so a healthy run stays quiet at this cadence regardless.
const DIAGNOSTICS_LOG_INTERVAL: Duration = Duration::from_secs(5);

/// How long the OSC threads' blocking receive waits before waking to re-check its stop flag. A
/// datagram or a Message returns the receive at once, so this is shutdown latency only, never
/// added latency on the I/O path.
const STOP_POLL: Duration = Duration::from_millis(100);

/// Which instrument the engine starts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Instrument {
    /// The built-in default rig ([`DEFAULT_JSON`]), its nested references resolved against the
    /// current directory.
    Default,
    /// An instrument document on disk. Its relative resources resolve against its own directory.
    Path(PathBuf),
    /// Nothing open and nothing playing, as an `open` of a document that does not load leaves the
    /// engine, but with no open instrument: `get_document` answers no source and an empty content
    /// hash, and an edit is refused until an `open` names an instrument. An `open` resolves against
    /// the current directory, then the library root.
    Silent,
}

/// Everything [`start`] needs. [`EngineConfig::new`] is `reuben play` with no flags.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub instrument: Instrument,
    /// The library root a reference missing beside its referencing file is looked up under.
    pub instrument_root: Option<PathBuf>,
    /// Device selection, channel maps and rate/buffer preferences.
    pub profile: DeviceProfile,
    pub block_size: usize,
    /// Where the OSC-in UDP listener binds; `None` opens no listener.
    pub osc_in: Option<String>,
    /// The `host:port` an `osc_out` node's Messages are sent to; `None` drops them.
    pub osc_out: Option<String>,
    /// Where the engine-channel server binds; `None` serves no socket, leaving only
    /// [`RunningEngine::channel`]. A non-loopback address takes a [`token`](Self::token).
    pub engine_addr: Option<String>,
    /// What a peer on another machine must open its connection with. Loopback peers need none.
    pub token: Option<Token>,
    /// Log every OSC message received and sent to stdout. Off by default: the stdout lock on the
    /// I/O paths adds jitter.
    pub log_osc: bool,
    /// Which swaps arriving over the engine-channel socket are held for the host's user rather than
    /// installed. `None` holds none, which is `reuben play`.
    pub swap_hold: Option<SwapHold>,
    /// Reload the open instrument when something outside the engine writes its files. On for
    /// `reuben play`; off for a host whose user decides what plays, since a reload installs without
    /// asking [`swap_hold`](Self::swap_hold).
    pub follow_files: bool,
}

impl EngineConfig {
    /// `instrument` on the default devices, OSC-in on every interface at [`DEFAULT_OSC_PORT`], no
    /// OSC-out, and the engine channel on [`DEFAULT_ENGINE_ADDR`] where the sidecar dials it.
    pub fn new(instrument: Instrument) -> Self {
        Self {
            instrument,
            instrument_root: None,
            profile: DeviceProfile::default(),
            block_size: DEFAULT_BLOCK_SIZE,
            osc_in: Some(format!("0.0.0.0:{DEFAULT_OSC_PORT}")),
            osc_out: None,
            engine_addr: Some(DEFAULT_ENGINE_ADDR.to_string()),
            token: None,
            log_osc: false,
            swap_hold: None,
            follow_files: true,
        }
    }
}

/// Why [`start`] failed. Every thread it had already started is stopped and joined, and every
/// socket it had bound is closed, before it returns.
#[derive(Debug)]
pub enum StartError {
    /// The instrument file could not be read.
    ReadInstrument { path: PathBuf, error: io::Error },
    /// The instrument would not load, or loaded and would not plan.
    Instrument(FromDocumentError),
    /// The OSC-in listener could not bind.
    OscIn { addr: String, error: io::Error },
    /// The OSC-out socket could not bind or connect to its target.
    OscOut { target: String, error: io::Error },
    /// The audio device (or the input device the instrument asked for) would not open.
    Audio(AudioError),
    /// The engine address names a non-loopback interface and the config has no token. The
    /// channel edits and opens instruments, so it never serves another machine without one.
    ChannelNeedsToken(String),
    /// The headless render thread would not start.
    RenderThread(io::Error),
    /// The thread that names dropped sends to subscribers would not start.
    DroppedSendWatch(io::Error),
    /// The thread that keeps the MIDI routes in step with what plays would not start.
    RouteWatch(io::Error),
    /// A `midi.inputs` entry of the profile matches no MIDI input port, or its port did not open.
    MidiDevice(String),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::ReadInstrument { path, error } => {
                write!(f, "read {}: {error}", path.display())
            }
            StartError::Instrument(e) => write!(f, "{e}"),
            StartError::OscIn { addr, error } => write!(f, "bind OSC-in {addr}: {error}"),
            StartError::OscOut { target, error } => write!(f, "OSC-out {target}: {error}"),
            StartError::Audio(e) => write!(f, "start audio: {e}"),
            StartError::ChannelNeedsToken(addr) => write!(
                f,
                "engine channel {addr} is not a loopback address, so it needs a token"
            ),
            StartError::RenderThread(e) => write!(f, "start the headless render thread: {e}"),
            StartError::DroppedSendWatch(e) => {
                write!(f, "start the dropped-sends watch thread: {e}")
            }
            StartError::RouteWatch(e) => write!(f, "start the MIDI routes thread: {e}"),
            StartError::MidiDevice(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for StartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StartError::ReadInstrument { error, .. }
            | StartError::OscIn { error, .. }
            | StartError::OscOut { error, .. }
            | StartError::RenderThread(error)
            | StartError::DroppedSendWatch(error)
            | StartError::RouteWatch(error) => Some(error),
            StartError::Instrument(e) => Some(e),
            StartError::Audio(e) => Some(e),
            StartError::ChannelNeedsToken(_) | StartError::MidiDevice(_) => None,
        }
    }
}

/// A running engine: the audio streams, the OSC threads, the engine-channel server and the
/// Coordinator behind it.
///
/// Dropping it — or [`shutdown`](Self::shutdown), which also returns the final counters — tears
/// the session down in order and returns only once every thread it started has exited: the
/// watcher following outside edits to the instrument's files, the dropped-sends watch, the thread
/// keeping the MIDI routes in step with what plays, the MIDI input ports (each printing what it
/// read), the engine-channel server (its connections woken and joined), the OSC-in listener, the
/// audio streams (paused, then dropped — see [`Streams::pause`](audio::Streams::pause) for why
/// both), the OSC-out sender, then the diagnostics logger. Every port it bound is free again
/// afterwards.
///
/// Not `Send`: cpal's streams are not, on every host. Keep it on the thread that started it and
/// hand [`channel`](Self::channel)s to any other.
pub struct RunningEngine {
    state: Option<ChannelState>,
    host: Arc<NativeHost>,
    tree_watch: Option<TreeWatch>,
    dropped_sends: Option<DroppedSendWatch>,
    route_watch: Option<RouteWatch>,
    midi_inputs: Option<MidiInputs>,
    channel_server: Option<ChannelServer>,
    channel_error: Option<io::Error>,
    osc_in: Option<OscListener>,
    output: Option<Output>,
    osc_out: Option<OscSender>,
    logger: Option<PeriodicLogger>,
    diagnostics: Arc<Diagnostics>,
    sample_rate: f32,
    block_size: usize,
    warnings: Vec<LoadWarning>,
    opened_input_channels: usize,
}

/// What keeps the render side running: the device streams, or the headless stand-in.
enum Output {
    Device(audio::Streams),
    // Held only to be dropped, which stops and joins it.
    #[allow(dead_code)]
    Headless(HeadlessDriver),
}

impl RunningEngine {
    /// A engine channel to this engine with no socket in between — the same verbs, the same
    /// Coordinator and the same answers as a client dialing [`engine_addr`](Self::engine_addr).
    ///
    /// Each call runs on the caller's thread: a swap validates and builds the new Engine there,
    /// behind any swap already holding the Coordinator lock, so a UI thread should not make it.
    /// The channel is owned and `Send` for exactly that — move it to a worker. One kept past
    /// shutdown fails every call as unreachable, the way a dead socket would, and keeps the
    /// Coordinator's memory until it drops.
    pub fn channel(&self) -> Channel {
        let state = self
            .state
            .as_ref()
            .expect("state is present until teardown");
        Channel::new(InProcess::new(state.clone()))
    }

    /// The device sample rate the engine renders at.
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// The core render block size.
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// The initial instrument's non-fatal load warnings (a missing sample degrades to silence).
    pub fn warnings(&self) -> &[LoadWarning] {
        &self.warnings
    }

    /// Where the OSC-in listener is bound, or `None` when none was asked for.
    pub fn osc_in_addr(&self) -> Option<SocketAddr> {
        self.osc_in.as_ref().map(|l| l.addr)
    }

    /// The `host:port` outbound OSC is sent to, or `None` when none was asked for.
    pub fn osc_out_target(&self) -> Option<&str> {
        self.osc_out.as_ref().map(|o| o.target.as_str())
    }

    /// Where the engine-channel server is bound, or `None` when none was asked for or it failed to bind
    /// (see [`channel_error`](Self::channel_error)).
    pub fn engine_addr(&self) -> Option<SocketAddr> {
        self.channel_server.as_ref().map(ChannelServer::local_addr)
    }

    /// Why the engine-channel server did not bind. Non-fatal: the engine plays and
    /// [`channel`](Self::channel) still reaches it; only socket clients (the sidecar) cannot.
    pub fn channel_error(&self) -> Option<&io::Error> {
        self.channel_error.as_ref()
    }

    /// Logical input channels the input stream provides: as many as the instrument the engine
    /// started on binds, fixed until the engine stops. `0` when no input stream opened.
    pub fn opened_input_channels(&self) -> usize {
        self.opened_input_channels
    }

    /// What the engine has installed now, read without waiting. Its input fits the input stream
    /// when [`input_stream_fits`](crate::engine_channel::input_stream_fits) says so for its
    /// `input_channels` and [`opened_input_channels`](Self::opened_input_channels).
    pub fn installed(&self) -> Installed {
        match &self.state {
            Some(state) => state.try_installed(),
            None => Installed::Poisoned,
        }
    }

    /// Offer `dir` to clients from now on, beside the library root and the open instrument's
    /// directory: what it holds is listed, opened and edited as theirs are, and a client may create
    /// an instrument in it.
    pub fn offer_dir(&self, dir: &Path) {
        self.host.offer_dir(dir);
    }

    /// The running counters.
    pub fn diagnostics(&self) -> Snapshot {
        self.diagnostics.snapshot()
    }

    /// Stop everything (see the type doc for the order) and return the final counters.
    pub fn shutdown(mut self) -> Snapshot {
        self.teardown();
        self.diagnostics.snapshot()
    }

    fn teardown(&mut self) {
        // Before the state closes, so no reload is mid-swap into a Coordinator being torn down.
        self.tree_watch = None;
        self.dropped_sends = None;
        self.route_watch = None;
        self.midi_inputs = None;
        // Next, so a channel a caller still holds is unreachable from here on rather than
        // answering from a Coordinator whose render side is about to go.
        if let Some(state) = &self.state {
            state.close();
        }
        if let Some(server) = self.channel_server.take() {
            server.shutdown();
        }
        // The last ChannelState this handle holds; dropping it drops the Coordinator (and any
        // retired Engine it still holds) here, off the audio thread, unless a caller kept a channel.
        self.state = None;
        if let Some(Output::Device(streams)) = &self.output {
            streams.pause();
        }
        self.output = None;
        // After the render side, so the UDP thread's producer is the last owner of its control
        // rings and frees them, with any batches still in them, on its own thread.
        self.osc_in = None;
        // Stopped by its own flag rather than by its sender dropping with the render callback,
        // which the pause above does not guarantee either.
        self.osc_out = None;
        self.logger = None;
    }
}

impl Drop for RunningEngine {
    fn drop(&mut self) {
        self.teardown();
    }
}

/// Start the engine on the audio device `config.profile` selects. See the [module docs](self).
///
/// The engine-channel server failing to bind is not an error — audio is the primary function — and is
/// reported by [`RunningEngine::channel_error`]. Everything else that fails is.
pub fn start(config: EngineConfig) -> Result<RunningEngine, StartError> {
    start_with(config, Render::Device)
}

/// [`start`] with no audio device: a thread renders blocks at
/// [`HEADLESS_SAMPLE_RATE`] and [`HEADLESS_BLOCK_SIZE`] and discards the output. Everything else —
/// the instrument load, the OSC sockets, the MIDI inputs, the engine-channel server, the teardown —
/// is [`start`]'s. The profile's audio settings and `block_size` are ignored, and no audio input
/// device opens: bound input pipes read silence.
pub fn start_headless(config: EngineConfig) -> Result<RunningEngine, StartError> {
    start_with(
        config,
        Render::Headless {
            config: headless_config(),
            pace: Pace::RealTime,
            outbound: None,
        },
    )
}

/// The render configuration [`start_headless`] runs at.
pub(crate) fn headless_config() -> AudioConfig {
    AudioConfig::new(HEADLESS_SAMPLE_RATE, HEADLESS_BLOCK_SIZE)
}

/// How often a [`DroppedSendWatch`] reads the stale-send count: the most a subscriber waits to
/// hear of a drop. Between drops a tick is one atomic load.
const DROPPED_SEND_POLL: Duration = Duration::from_millis(20);

/// A thread that names, to the engine channel's subscribers, every compiled send the render side
/// dropped because a swap installed between its compile and its apply. It reads the count the
/// render side keeps, and when it moves asks the state to publish
/// ([`ChannelState::publish_dropped_sends`]), which reads the addresses back off the returned
/// batches. Dropping it stops and joins the thread.
pub(crate) struct DroppedSendWatch {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl DroppedSendWatch {
    pub(crate) fn spawn(state: ChannelState, diagnostics: Arc<Diagnostics>) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("dropped-sends".to_string())
            .spawn(move || {
                let mut seen = diagnostics.load_stale_sends_dropped();
                while !thread_stop.load(Ordering::SeqCst) {
                    std::thread::park_timeout(DROPPED_SEND_POLL);
                    let now = diagnostics.load_stale_sends_dropped();
                    if now != seen {
                        seen = now;
                        state.publish_dropped_sends();
                    }
                }
            })?;
        Ok(Self {
            stop,
            handle: Some(handle),
        })
    }
}

impl Drop for DroppedSendWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            handle.thread().unpark();
            let _ = handle.join();
        }
    }
}

/// How often the [`HeadlessDriver`] renders a block.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Pace {
    /// One block's duration per block, against a running deadline so sleep error does not
    /// accumulate: what a device would ask for.
    RealTime,
    /// A fixed sleep after each block, whatever the block's duration.
    Every(Duration),
}

/// The render driver of a headless engine: a thread that owns the [`RenderSlot`] a device callback
/// would and drives it in a loop, so everything a device exercises runs with no device — the
/// install mailbox drains, the master-gain ramp runs, survivors are box-transplanted, and retirees
/// are posted back for the Coordinator's off-thread `reclaim`. It renders the *logical* master; the
/// device output map needs a device.
///
/// It drains the control rings as `audio.rs` does, batch by batch, each batch whole and handed back
/// to its producer, and forwards outbound Messages to the OSC-out sender. Dropping it stops and
/// joins the thread.
pub(crate) struct HeadlessDriver {
    stop: Arc<AtomicBool>,
    #[cfg_attr(not(test), allow(dead_code))]
    blocks: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
}

impl HeadlessDriver {
    /// Start driving `side` in blocks of `config.block_size`, paced by `pace`, counting into
    /// `diagnostics` what the device callback counts. Each control batch is shown to `drained` once
    /// it has been queued, on the render thread, before it goes back to its producer; the engine's
    /// own driver passes one that does nothing.
    pub(crate) fn spawn(
        side: RenderSide,
        config: AudioConfig,
        pace: Pace,
        mut control: ControlDrain,
        outbound: Option<Sender<Message>>,
        diagnostics: Arc<Diagnostics>,
        drained: impl FnMut(&ControlBatch) + Send + 'static,
    ) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let blocks = Arc::new(AtomicU64::new(0));
        let blocks_thread = Arc::clone(&blocks);
        let block_size = config.block_size;
        let block = Duration::from_secs_f64(block_size as f64 / f64::from(config.sample_rate));
        let max_lag = block * HEADLESS_MAX_LAG_BLOCKS;
        let handle = std::thread::Builder::new()
            .name("headless-render".to_string())
            .spawn(move || {
                let mut slot = RenderSlot::new(side);
                let mut buf = vec![0.0f32; block_size * slot.channels().max(1)];
                let mut deadline = Instant::now();
                let mut drained = drained;
                while !stop_thread.load(Ordering::SeqCst) {
                    // Each item is a whole batch, applied without splitting, so a gesture reaches
                    // one block. Flat args in, typed at the slot's Engine where the destination
                    // port's type is known.
                    drain_control_each(&mut control, &mut slot, &diagnostics, &mut drained);
                    let ch = slot.channels().max(1);
                    // A swap can change the channel count; the device callback is handed a buffer
                    // of the right size instead.
                    if buf.len() != block_size * ch {
                        buf.resize(block_size * ch, 0.0);
                    }
                    slot.fill(&mut buf);
                    diagnostics.record_wired_writes_dropped(slot.take_dropped_wired_writes());
                    diagnostics
                        .record_control_messages_dropped_full(slot.take_dropped_queue_full());
                    diagnostics.record_unprepared_writes_dropped(slot.take_dropped_unprepared());
                    diagnostics.record_operator_counts(slot.take_operator_counts());
                    for m in slot.drain_outbound() {
                        if let Some(tx) = &outbound {
                            let _ = tx.send(m);
                        }
                    }
                    blocks_thread.fetch_add(1, Ordering::Relaxed);
                    match pace {
                        Pace::RealTime => {
                            deadline += block;
                            let now = Instant::now();
                            if deadline > now {
                                std::thread::sleep(deadline - now);
                            } else if now - deadline > max_lag {
                                deadline = now;
                            }
                        }
                        Pace::Every(wait) => std::thread::sleep(wait),
                    }
                }
                // The slot (and its Engine + mailbox) drops here, off any RT thread.
            })?;
        Ok(Self {
            stop,
            blocks,
            handle: Some(handle),
        })
    }

    /// How many blocks the driver has rendered.
    #[cfg(test)]
    pub(crate) fn blocks(&self) -> u64 {
        self.blocks.load(Ordering::Relaxed)
    }
}

impl Drop for HeadlessDriver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// How the render side is driven.
pub(crate) enum Render<'a> {
    /// A cpal output stream on the profile's device.
    Device,
    /// No device: a [`HeadlessDriver`] drives the render side at `config`, feeding no input; the input
    /// stream it reports is as wide as a device start would open it.
    Headless {
        config: AudioConfig,
        pace: Pace,
        /// When `Some`, receives a clone of the render callback's outbound sender — a test's
        /// stand-in for a device callback that outlives its stream's drop.
        outbound: Option<&'a mut Option<Sender<Message>>>,
    },
}

pub(crate) fn start_with(
    config: EngineConfig,
    render: Render<'_>,
) -> Result<RunningEngine, StartError> {
    let EngineConfig {
        instrument,
        instrument_root,
        profile,
        block_size,
        osc_in,
        osc_out,
        engine_addr,
        token,
        log_osc,
        swap_hold,
        follow_files,
    } = config;

    let engine_addr = match engine_addr {
        Some(addr) => Some(resolve_engine_addr(&addr, token.is_some())?),
        None => None,
    };
    let library_root = instrument_root.clone();
    let nothing_open = instrument == Instrument::Silent;
    let (instrument_json, resolver, initial_source) = read_instrument(instrument, instrument_root)?;

    // The counters every producer and the render side feed, made first because the control
    // producers count their drops into it.
    let diagnostics = Diagnostics::new();

    // The control ingress into the render callback: a ring pair for the UDP listener (the foreign
    // edge) and one the engine channel's `send` connections share. A `send` and an external
    // datagram are indistinguishable downstream.
    let (udp_control, udp_ring) = control::ring_pair(Arc::clone(&diagnostics));
    let (send_control, send_ring) = control::ring_pair(Arc::clone(&diagnostics));
    // And one for the MIDI router, which the MIDI devices and `send_midi` share behind its lock.
    let (midi_control, midi_ring) = control::ring_pair(Arc::clone(&diagnostics));
    let control = ControlDrain::new(vec![udp_ring, send_ring, midi_ring]);
    let midi_router: SharedRouter = Arc::new(Mutex::new(Router::new(
        midi_control,
        Arc::clone(&diagnostics),
        log_osc,
    )));
    // Before any device or socket opens, so a profile naming a port that is not there fails fast.
    let midi_inputs = MidiInputs::open(&profile.midi.inputs, Arc::clone(&midi_router))
        .map_err(StartError::MidiDevice)?;

    let (osc_out_tx, osc_out) = match osc_out {
        Some(target) => {
            let (tx, sender) = OscSender::start(target, log_osc)?;
            (Some(tx), Some(sender))
        }
        None => (None, None),
    };

    let osc_in = match osc_in {
        Some(addr) => Some(OscListener::bind(addr, udp_control, log_osc)?),
        None => None,
    };

    // One store for the session: the Coordinator resolves through a clone of it, and the
    // engine channel's `call` writes through the same one, so an edit waiting in memory is what
    // the check before it goes live loads.
    let staged: Staged<Box<dyn Resources + Send + Sync>> = Staged::new(Box::new(resolver));
    let store = Arc::new(DocumentHistory::new(staged.clone()));
    let build =
        |cfg: AudioConfig| reuben_api::engine::install_initial(&instrument_json, staged, cfg);
    let (
        output,
        diagnostics,
        meters,
        coordinator,
        render_config,
        warnings,
        audio_config,
        opened_input,
    ): (_, _, _, _, Arc<dyn RenderConfigPublisher>, _, _, _) = match render {
        Render::Device => {
            let mut negotiated = None;
            let live = audio::start(
                control,
                Arc::clone(&diagnostics),
                block_size,
                osc_out_tx,
                &profile,
                |cfg| {
                    negotiated = Some(cfg);
                    build(cfg)
                },
            )
            .map_err(|e| match e {
                AudioError::Instrument(e) => StartError::Instrument(e),
                e => StartError::Audio(e),
            })?;
            let audio_config = negotiated.expect("audio::start builds before it returns Ok");
            (
                Output::Device(live.streams),
                live.diagnostics,
                live.meters,
                live.coordinator,
                live.render_config,
                live.warnings,
                audio_config,
                live.opened_input_channels,
            )
        }
        Render::Headless {
            config: cfg,
            pace,
            outbound,
        } => {
            let (coordinator, side, warnings) = build(cfg).map_err(StartError::Instrument)?;
            if let Some(slot) = outbound {
                *slot = osc_out_tx.clone();
            }
            // Sized as a device start sizes its input stream, so a swap is judged against the
            // same geometry. No input is fed: the bound pipes read silence.
            let opened_input = coordinator.installed_input_channels();
            let driver = HeadlessDriver::spawn(
                side,
                cfg,
                pace,
                control,
                osc_out_tx,
                Arc::clone(&diagnostics),
                |_| {},
            )
            .map_err(StartError::RenderThread)?;
            (
                Output::Headless(driver),
                Arc::clone(&diagnostics),
                Meters::none(),
                coordinator,
                Arc::new(HeadlessRenderConfig {
                    opened_input_channels: opened_input,
                }),
                warnings,
                cfg,
                opened_input,
            )
        }
    };
    let logger =
        diagnostics::spawn_periodic_logger(Arc::clone(&diagnostics), DIAGNOSTICS_LOG_INTERVAL);

    // The engine channel owns the Coordinator — the single writer of graph structure — and
    // publishes each swap's device output map through `render_config`.
    let host = Arc::new(
        NativeHost::new(Arc::clone(&diagnostics), send_control)
            .with_meters(meters)
            .with_render_config(render_config)
            .with_swap_hold(swap_hold)
            .with_library_root(library_root)
            .with_midi_router(Arc::clone(&midi_router)),
    );
    diagnostics.watch_retire_queue(coordinator.retired());
    let state =
        ChannelState::new(coordinator, host.clone(), store).with_installed_source(initial_source);
    let state = if nothing_open {
        state.with_nothing_open()
    } else {
        state
    };
    let (channel_server, channel_error) = match engine_addr {
        Some(Ok(addrs)) => match ChannelServer::bind_with_token(&addrs[..], state.clone(), token) {
            Ok(server) => (Some(server), None),
            Err(e) => (None, Some(e)),
        },
        Some(Err(e)) => (None, Some(e)),
        None => (None, None),
    };
    let tree_watch = follow_files.then(|| tree_watch::spawn(state.clone()));
    let dropped_sends = DroppedSendWatch::spawn(state.clone(), Arc::clone(&diagnostics))
        .map_err(StartError::DroppedSendWatch)?;
    let route_watch = RouteWatch::spawn(
        Channel::new(InProcess::new(state.clone())),
        midi_router,
        follow_files,
        host.midi_map_generation(),
    )
    .map_err(StartError::RouteWatch)?;

    Ok(RunningEngine {
        state: Some(state),
        host,
        tree_watch,
        dropped_sends: Some(dropped_sends),
        route_watch: Some(route_watch),
        midi_inputs: Some(midi_inputs),
        channel_server,
        channel_error,
        osc_in,
        output: Some(output),
        osc_out,
        logger: Some(logger),
        diagnostics,
        sample_rate: audio_config.sample_rate,
        block_size: audio_config.block_size,
        warnings,
        opened_input_channels: opened_input,
    })
}

/// Resolve the engine address once, refusing one that resolves to anything but loopback unless
/// the engine has a token. The inner error is a resolution failure, which is non-fatal like a
/// failed bind.
fn resolve_engine_addr(
    addr: &str,
    has_token: bool,
) -> Result<io::Result<Vec<SocketAddr>>, StartError> {
    let resolved = match addr.to_socket_addrs() {
        Ok(resolved) => resolved.collect::<Vec<_>>(),
        Err(e) => return Ok(Err(e)),
    };
    if !has_token
        && resolved
            .iter()
            .any(|candidate| !candidate.ip().to_canonical().is_loopback())
    {
        return Err(StartError::ChannelNeedsToken(addr.to_string()));
    }
    Ok(Ok(resolved))
}

/// The instrument's JSON, the resolver its references resolve through, and the source name
/// `get_document` reports for it: the path made absolute against the working directory, `None`
/// for the built-in rig and for silence, which have no source to name.
///
/// The Coordinator owns the resolver for the session, and every swap resolves through it. Its base
/// directory is the anchor for this document; an open hands the opened file's own path in as the
/// referrer, so that document resolves from its own directory. The library root is the fallback
/// for both.
fn read_instrument(
    instrument: Instrument,
    root: Option<PathBuf>,
) -> Result<(String, FsResolver, Option<String>), StartError> {
    let (json, resolver, source) = match instrument {
        Instrument::Path(path) => {
            let json =
                std::fs::read_to_string(&path).map_err(|error| StartError::ReadInstrument {
                    path: path.clone(),
                    error,
                })?;
            // Absolute, because a client reading it back may run in another directory, and
            // normalized as every name the server publishes is.
            let source = crate::engine_channel::lexical_absolute(&path)
                .unwrap_or_else(|| path.clone())
                .display()
                .to_string();
            (json, FsResolver::for_instrument(&path), Some(source))
        }
        Instrument::Default => (DEFAULT_JSON.to_string(), FsResolver::new("."), None),
        Instrument::Silent => (SILENCE.to_string(), FsResolver::new("."), None),
    };
    let resolver = match root {
        Some(root) => resolver.with_root(root),
        None => resolver,
    };
    Ok((json, resolver, source))
}

/// The OSC-out sender: encodes and sends each outbound Message off the audio thread. Dropping it
/// stops and joins the thread, which closes the socket.
struct OscSender {
    target: String,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl OscSender {
    /// Bind the socket to `target` and start the thread; the returned sender is the render
    /// callback's end.
    fn start(target: String, log_osc: bool) -> Result<(Sender<Message>, Self), StartError> {
        let fail = |error| StartError::OscOut {
            target: target.clone(),
            error,
        };
        let socket = UdpSocket::bind("0.0.0.0:0").map_err(fail)?;
        socket.connect(&target).map_err(fail)?;
        let (tx, rx) = mpsc::channel::<Message>();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("osc-out".to_string())
            .spawn(move || {
                let mut flat = Vec::new();
                while !thread_stop.load(Ordering::SeqCst) {
                    let m = match rx.recv_timeout(STOP_POLL) {
                        Ok(m) => m,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    flat.clear();
                    // `false`: the Arg has no OSC form and expanded to nothing, so no datagram.
                    if !render::osc_out_args(&m.arg, &mut flat) {
                        continue;
                    }
                    match osc::encode(&m.address, &flat) {
                        Ok(bytes) => {
                            if log_osc {
                                println!("send {} {:?}", m.address, flat);
                            }
                            let _ = socket.send(&bytes);
                        }
                        Err(e) => eprintln!("OSC encode error: {e}"),
                    }
                }
            })
            .expect("spawn osc-out thread");
        Ok((
            tx,
            Self {
                target,
                stop,
                handle: Some(handle),
            },
        ))
    }
}

impl Drop for OscSender {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The OSC-in UDP listener: decodes each datagram into one [`ControlBatch`] and pushes it onto its
/// own ring pair into the render callback, decoding into a batch the callback returned when there is
/// one. Dropping it stops and joins the thread, which closes the socket.
struct OscListener {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl OscListener {
    fn bind(addr: String, mut control: ControlProducer, log_osc: bool) -> Result<Self, StartError> {
        let fail = |error| StartError::OscIn {
            addr: addr.clone(),
            error,
        };
        let socket = UdpSocket::bind(addr.as_str()).map_err(fail)?;
        socket.set_read_timeout(Some(STOP_POLL)).map_err(fail)?;
        let local = socket.local_addr().map_err(fail)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("osc-in".to_string())
            .spawn(move || {
                // One datagram is one batch; this buffer is also what bounds a batch's size.
                let mut buf = [0u8; 1024];
                while !thread_stop.load(Ordering::SeqCst) {
                    match socket.recv_from(&mut buf) {
                        Ok((n, _)) => {
                            let mut batch = control.batch();
                            match osc::decode_into(&buf[..n], &mut batch) {
                                Ok(()) => {
                                    if log_osc {
                                        for m in &batch {
                                            println!("recv {} {:?}", m.address, m.args.as_slice());
                                        }
                                    }
                                    // A full pair drops and counts the datagram's batch.
                                    let _ = control.push(batch);
                                }
                                Err(e) => eprintln!("OSC decode error: {e}"),
                            }
                        }
                        // The poll timeout, or a signal landing on this thread: a read timeout
                        // makes Linux return EINTR even under SA_RESTART.
                        Err(ref e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::WouldBlock
                                    | io::ErrorKind::TimedOut
                                    | io::ErrorKind::Interrupted
                            ) => {}
                        // Not fatal to the listener: Windows fails the receive of an oversized
                        // datagram (and reports an earlier send's ICMP unreachable here) where
                        // Linux truncates. The sleep keeps a persistent error from spinning.
                        Err(e) => {
                            eprintln!("OSC recv error: {e}");
                            std::thread::sleep(STOP_POLL);
                        }
                    }
                }
            })
            .expect("spawn osc-in thread");
        Ok(Self {
            addr: local,
            stop,
            handle: Some(handle),
        })
    }
}

impl Drop for OscListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A production headless engine renders at the rate a device would ask for, not as fast as it
    /// can: over a stretch of wall time, the blocks it rendered are about that time's worth.
    #[test]
    fn a_headless_engine_renders_at_real_time() {
        let config = EngineConfig {
            osc_in: None,
            engine_addr: None,
            follow_files: false,
            ..EngineConfig::new(Instrument::Default)
        };
        let engine = start_headless(config).expect("the engine starts");
        let blocks = || match &engine.output {
            Some(Output::Headless(driver)) => driver.blocks(),
            _ => panic!("a headless engine has a headless driver"),
        };
        // Past the first block, so thread start-up is not in the measured stretch.
        while blocks() == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
        let (from, started) = (blocks(), Instant::now());
        std::thread::sleep(Duration::from_millis(500));
        let rendered = (blocks() - from) as f64;
        let elapsed = started.elapsed().as_secs_f64();
        let expected = elapsed * f64::from(HEADLESS_SAMPLE_RATE) / HEADLESS_BLOCK_SIZE as f64;
        // Loose below: a loaded runner can stall the driver past its lag cap, which drops time.
        // Tight above: that is what tells real-time pacing from rendering as fast as it can.
        assert!(
            rendered >= expected * 0.5 && rendered <= expected * 1.25,
            "rendered {rendered} blocks in {elapsed:.3}s; real time is {expected:.1}"
        );
    }
}

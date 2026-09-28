//! The engine entry point: everything `reuben play` starts, as one call another binary can make.
//!
//! [`start`] loads the instrument, opens the audio device (and the input device when the
//! instrument binds input channels) per the [`DeviceProfile`], binds the OSC-in UDP listener and
//! the optional OSC-out sender, and serves the loopback structure channel the MCP sidecar dials.
//! The [`RunningEngine`] it returns is the whole session: it reaches the engine in-process through
//! the window's own [`Channel`], and dropping it stops every thread and frees every port.
//!
//! In-process and over the socket are the same channel, not two paths: [`RunningEngine::channel`]
//! is a [`Channel`] over [`InProcess`], which dispatches into the very [`StructureState`] the
//! [`StructureServer`] serves. A caller drives it with the window's engine verbs
//! ([`swap_instrument`](reuben_api::engine::swap_instrument),
//! [`send_live_controls`](reuben_api::engine::send_live_controls),
//! [`get_engine_status`](reuben_api::engine::get_engine_status), …), and a swap made in-process is
//! the document the sidecar reads next, because there is one Coordinator behind both.

use std::fmt;
use std::io;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use reuben_api::engine::{
    Channel, FromDocumentError, InProcess, LoadWarning, StructureState, DEFAULT_STRUCTURE_ADDR,
};
use reuben_api::render::{self, AudioConfig, Message};
use reuben_api::FsResolver;

use crate::audio::{self, AudioError};
use crate::diagnostics::{self, Diagnostics, PeriodicLogger, Snapshot};
use crate::osc::{self, ControlBatch, DEFAULT_OSC_PORT};
use crate::profile::DeviceProfile;
use crate::rigs::DEFAULT_JSON;
use crate::structure::{HeadlessRenderConfig, NativeHost, RenderConfigPublisher, StructureServer};
use crate::test_support::FakeCallback;

/// The core render block size `reuben play` runs at.
pub const DEFAULT_BLOCK_SIZE: usize = 256;

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
    /// Where the loopback structure server binds; `None` serves no socket, leaving only
    /// [`RunningEngine::channel`]. Must be a loopback address: structure edits are more powerful
    /// than control and the channel must never be network-exposed.
    pub structure: Option<String>,
    /// Log every OSC message received and sent to stdout. Off by default: the stdout lock on the
    /// I/O paths adds jitter.
    pub log_osc: bool,
}

impl EngineConfig {
    /// `instrument` on the default devices, OSC-in on every interface at [`DEFAULT_OSC_PORT`], no
    /// OSC-out, and the structure channel on [`DEFAULT_STRUCTURE_ADDR`] where the sidecar dials it.
    pub fn new(instrument: Instrument) -> Self {
        Self {
            instrument,
            instrument_root: None,
            profile: DeviceProfile::default(),
            block_size: DEFAULT_BLOCK_SIZE,
            osc_in: Some(format!("0.0.0.0:{DEFAULT_OSC_PORT}")),
            osc_out: None,
            structure: Some(DEFAULT_STRUCTURE_ADDR.to_string()),
            log_osc: false,
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
    /// The structure address names a non-loopback interface. Structure edits are more powerful
    /// than control, so the channel is never network-exposed.
    StructureNotLoopback(String),
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
            StartError::StructureNotLoopback(addr) => {
                write!(f, "structure channel {addr} is not a loopback address")
            }
        }
    }
}

impl std::error::Error for StartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StartError::ReadInstrument { error, .. }
            | StartError::OscIn { error, .. }
            | StartError::OscOut { error, .. } => Some(error),
            StartError::Instrument(e) => Some(e),
            StartError::Audio(e) => Some(e),
            StartError::StructureNotLoopback(_) => None,
        }
    }
}

/// A running engine: the audio streams, the OSC threads, the structure server and the
/// Coordinator behind it.
///
/// Dropping it — or [`shutdown`](Self::shutdown), which also returns the final counters — tears
/// the session down in order and returns only once every thread it started has exited: the
/// structure server (its connections woken and joined), the OSC-in listener, the audio streams
/// (paused, then dropped — see [`Streams::pause`](audio::Streams::pause) for why both), the OSC-out
/// sender, then the diagnostics logger. Every port it bound is free again afterwards.
///
/// Not `Send`: cpal's streams are not, on every host. Keep it on the thread that started it and
/// hand [`channel`](Self::channel)s to any other.
pub struct RunningEngine {
    state: Option<StructureState>,
    structure: Option<StructureServer>,
    structure_error: Option<io::Error>,
    osc_in: Option<OscListener>,
    output: Option<Output>,
    osc_out: Option<OscSender>,
    logger: Option<PeriodicLogger>,
    diagnostics: Arc<Diagnostics>,
    sample_rate: f32,
    block_size: usize,
    warnings: Vec<LoadWarning>,
}

/// What keeps the render side running: the device streams, or the headless stand-in.
enum Output {
    Device(audio::Streams),
    // Held only to be dropped, which stops and joins it.
    #[allow(dead_code)]
    Headless(FakeCallback),
}

impl RunningEngine {
    /// A structure channel to this engine with no socket in between — the same verbs, the same
    /// Coordinator and the same answers as a client dialing [`structure_addr`](Self::structure_addr).
    ///
    /// Each call runs on the caller's thread: a swap validates and builds the new Engine there,
    /// behind any swap already holding the Coordinator lock, so a UI thread should not make it.
    /// The channel is owned and `Send` for exactly that — move it to a worker. One kept past
    /// shutdown keeps the Coordinator alive until it drops; its `send` is then refused (the control
    /// ingress is gone) and a swap installs into a mailbox nothing drains.
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

    /// Where the structure server is bound, or `None` when none was asked for or it failed to bind
    /// (see [`structure_error`](Self::structure_error)).
    pub fn structure_addr(&self) -> Option<SocketAddr> {
        self.structure.as_ref().map(StructureServer::local_addr)
    }

    /// Why the structure server did not bind. Non-fatal: the engine plays and
    /// [`channel`](Self::channel) still reaches it; only socket clients (the sidecar) cannot.
    pub fn structure_error(&self) -> Option<&io::Error> {
        self.structure_error.as_ref()
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
        if let Some(server) = self.structure.take() {
            server.shutdown();
        }
        // The last StructureState this handle holds; dropping it drops the Coordinator (and any
        // retired Engine it still holds) here, off the audio thread, unless a caller kept a channel.
        self.state = None;
        self.osc_in = None;
        if let Some(Output::Device(streams)) = &self.output {
            streams.pause();
        }
        self.output = None;
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
/// The structure server failing to bind is not an error — audio is the primary function — and is
/// reported by [`RunningEngine::structure_error`]. Everything else that fails is.
pub fn start(config: EngineConfig) -> Result<RunningEngine, StartError> {
    start_with(config, Render::Device)
}

/// How the render side is driven.
pub(crate) enum Render {
    /// A cpal output stream on the profile's device.
    Device,
    /// No device: [`FakeCallback`] drives the render side at this config, output-only.
    Headless(AudioConfig),
}

pub(crate) fn start_with(
    config: EngineConfig,
    render: Render,
) -> Result<RunningEngine, StartError> {
    let EngineConfig {
        instrument,
        instrument_root,
        profile,
        block_size,
        osc_in,
        osc_out,
        structure,
        log_osc,
    } = config;

    if let Some(addr) = &structure {
        require_loopback(addr)?;
    }
    let (instrument_json, resolver, initial_source) = read_instrument(instrument, instrument_root)?;

    // The one control ingress into the render callback. Two producers — the UDP listener (the
    // foreign edge) and the structure channel's `send` — each hold a clone, so a `send` and an
    // external datagram are indistinguishable downstream.
    let (osc_tx, osc_rx) = mpsc::channel::<ControlBatch>();

    let (osc_out_tx, osc_out) = match osc_out {
        Some(target) => {
            let (tx, sender) = OscSender::start(target, log_osc)?;
            (Some(tx), Some(sender))
        }
        None => (None, None),
    };

    let osc_in = match osc_in {
        Some(addr) => Some(OscListener::bind(addr, osc_tx.clone(), log_osc)?),
        None => None,
    };

    let build =
        |cfg: AudioConfig| reuben_api::engine::install_initial(&instrument_json, resolver, cfg);
    let (output, diagnostics, coordinator, render_config, warnings, audio_config): (
        _,
        _,
        _,
        Arc<dyn RenderConfigPublisher>,
        _,
        _,
    ) = match render {
        Render::Device => {
            let mut negotiated = None;
            let live = audio::start(osc_rx, block_size, osc_out_tx, &profile, |cfg| {
                negotiated = Some(cfg);
                build(cfg)
            })
            .map_err(|e| match e {
                AudioError::Instrument(e) => StartError::Instrument(e),
                e => StartError::Audio(e),
            })?;
            let audio_config = negotiated.expect("audio::start builds before it returns Ok");
            (
                Output::Device(live.streams),
                live.diagnostics,
                live.coordinator,
                live.render_config,
                live.warnings,
                audio_config,
            )
        }
        Render::Headless(cfg) => {
            let (coordinator, side, warnings) = build(cfg).map_err(StartError::Instrument)?;
            (
                Output::Headless(FakeCallback::drive(side, osc_rx, osc_out_tx)),
                Diagnostics::new(),
                coordinator,
                Arc::new(HeadlessRenderConfig {
                    opened_input_channels: 0,
                }),
                warnings,
                cfg,
            )
        }
    };
    let logger =
        diagnostics::spawn_periodic_logger(Arc::clone(&diagnostics), DIAGNOSTICS_LOG_INTERVAL);

    // The structure channel owns the Coordinator — the single writer of graph structure — and
    // publishes each swap's device output map through `render_config`.
    let host = NativeHost::new(Arc::clone(&diagnostics), osc_tx).with_render_config(render_config);
    let state =
        StructureState::new(coordinator, Arc::new(host)).with_installed_source(initial_source);
    let (structure, structure_error) = match structure {
        Some(addr) => match StructureServer::bind(addr.as_str(), state.clone()) {
            Ok(server) => (Some(server), None),
            Err(e) => (None, Some(e)),
        },
        None => (None, None),
    };

    Ok(RunningEngine {
        state: Some(state),
        structure,
        structure_error,
        osc_in,
        output: Some(output),
        osc_out,
        logger: Some(logger),
        diagnostics,
        sample_rate: audio_config.sample_rate,
        block_size: audio_config.block_size,
        warnings,
    })
}

/// Refuse a structure address that resolves to anything but loopback. One that does not resolve
/// is left to the bind, whose failure is non-fatal.
fn require_loopback(addr: &str) -> Result<(), StartError> {
    let Ok(resolved) = addr.to_socket_addrs() else {
        return Ok(());
    };
    for candidate in resolved {
        if !candidate.ip().is_loopback() {
            return Err(StartError::StructureNotLoopback(addr.to_string()));
        }
    }
    Ok(())
}

/// The instrument's JSON, the resolver its references resolve through, and the source name
/// `get_document` reports for it (`None` for the built-in rig, which has no source to name).
///
/// The Coordinator owns the resolver for the session and a by-path swap resolves through it too:
/// the session does not re-anchor per swap, so a swapped document's relative resources resolve
/// against this initial anchor plus the library root.
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
            let source = path.display().to_string();
            (json, FsResolver::for_instrument(&path), Some(source))
        }
        Instrument::Default => (DEFAULT_JSON.to_string(), FsResolver::new("."), None),
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

/// The OSC-in UDP listener: decodes each datagram into one [`ControlBatch`] and forwards it to the
/// render callback. Dropping it stops and joins the thread, which closes the socket.
struct OscListener {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl OscListener {
    fn bind(addr: String, tx: Sender<ControlBatch>, log_osc: bool) -> Result<Self, StartError> {
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
                        Ok((n, _)) => match osc::decode(&buf[..n]) {
                            Ok(batch) => {
                                if log_osc {
                                    for m in &batch {
                                        println!("recv {} {:?}", m.address, m.args.as_slice());
                                    }
                                }
                                let _ = tx.send(batch);
                            }
                            Err(e) => eprintln!("OSC decode error: {e}"),
                        },
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

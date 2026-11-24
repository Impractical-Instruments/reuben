//! The engine channel's **host side**: a TCP / NDJSON server, and the native seams the
//! window's verbs are served over.
//!
//! **TCP on `127.0.0.1`** by default, carrying **newline-delimited JSON**, one response per request
//! in order. A thread in `reuben play` owns it. It listens beyond loopback only when bound there
//! with a token, and then serves a peer on another machine only after that peer's first line
//! carries the token (see [`admit`]): structure edits are more powerful than control, unlike OSC's
//! open `0.0.0.0:9000`.
//!
//! What each verb *means* is not here: the envelope, the dispatch, the `expect` guard and the
//! retain-prior report are the window's, so this door and the sidecar cannot disagree about them.
//! What is here is what only a native host can supply — the listener and its threads, the device
//! output map, the render callback's liveness, the counters, and the control ingress:
//!
//! - the control ingress is a [`ControlProducer`] the connection threads share behind a lock only
//!   they take, beside the UDP decode thread's own, so a `send` and an external datagram are
//!   indistinguishable at the render callback and OSC-the-binary-protocol stays at `play`'s
//!   foreign edge;
//! - [`NativeHost::publish_render_config`] rebuilds the device output map off-thread against the
//!   *retained* device channel count (streams are fixed at `play` start) and ships it across the
//!   render mailbox, so the callback installs Engine + map together. A swapped-in engine binding
//!   input channels no open stream provides **dark-degrades to silence** with a loud swap-report
//!   warning, never an error or a crash;
//! - [`NativeHost::retire_poll`] supplies the wait the window's deferred free polls over — a gate
//!   that tells a running device from a stopped one, so a reclaim that will never land gives up at
//!   the liveness grace instead of holding the Coordinator lock to the full timeout.
//!
//! reuben-native stays **tokio-free** (the async runtime is fenced to reuben-mcp): the server is a
//! dedicated std thread doing blocking line I/O, never an async runtime.

use std::io::{BufReader, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use reuben_api::authoring::Diag;
use reuben_api::engine::{
    admit, dispatch, dispatch_request, read_ndjson_line, Admit, ChannelState, DiagnosticsReport,
    EngineHost, IngressClosed, ListWhat, MetersReport, PreparedMessage, Request, Response,
    Subscription, Token, TOKEN_REQUIRED,
};
use reuben_api::probe::ProbeSlots;
use reuben_api::render::Arg;
use reuben_api::surface::SurfaceKind;

use crate::control::ControlProducer;
use crate::diagnostics::{Diagnostics, Snapshot};
use crate::meters::Meters;
use crate::midi::router::SharedRouter;
use crate::midi::translate::parse_translated;
use crate::osc::OscIn;
use crate::probes::{spawn_lease_thread, Probes, LEASE, LEASE_CHECK};

/// How long the accept loop sleeps between polls of its shutdown flag while idle. The listener
/// is non-blocking so the loop can observe [`ChannelServer::shutdown`] without a client ever
/// connecting; this cadence is the shutdown latency, not a hot path (never the audio thread).
const ACCEPT_POLL: Duration = Duration::from_millis(50);

/// How long a connection handler's read blocks before waking to re-check its shutdown flag. A
/// [`shutdown`](ChannelServer::shutdown) does **not** interrupt a `read` already blocked on a
/// socket on Windows (unlike Unix, where the accept thread's `shutdown(Shutdown::Both)` unblocks
/// the peer's `read_line`); a blocked recv there strands the handler until an OS-level connection
/// timeout (~2 min), so every connection torn down while a client is still attached pays that.
/// A read timeout makes the wait self-interrupting on every platform: the handler polls the flag
/// on each timeout instead of trusting a cross-thread wake. On a stream socket a timeout only
/// fires with *no* bytes available (a partial line would have returned immediately), so it never
/// splits a request — the framing the blocking read gave us is preserved. This cadence is the
/// per-connection shutdown latency, not a hot path (never the audio thread).
const READ_POLL: Duration = Duration::from_millis(250);

/// How long a swap waits for the RT callback to drain the install mailbox and post the retired
/// Engine back before giving up the off-thread reclaim (the "engine isn't consuming
/// swaps; is audio running?" case). With a live callback the retiree comes home in one master-gain
/// ramp (~20ms); this bound only bites when audio has genuinely stopped — the swap has
/// already committed, so a timeout just defers the free to the next swap's opportunistic reclaim.
const SWAP_RECLAIM_TIMEOUT: Duration = Duration::from_millis(500);

/// How many callback periods of silence the swap's install/reclaim poll tolerates before concluding
/// audio has *stopped*. The grace is derived from the render callback's own
/// period rather than a fixed wall-clock so a large-buffer / low-rate device (whose callbacks are
/// legitimately far apart) is never mistaken for a stopped one: at 4× the period a live callback has
/// always ticked at least once within the window. See [`SwapPollGate`].
const SWAP_LIVENESS_GRACE_PERIODS: u32 = 4;

/// Floor under [`SWAP_LIVENESS_GRACE_PERIODS`] × period, so a tiny buffer still gets a grace wide
/// enough to absorb poll-thread scheduling jitter, and a publisher that reports no period (headless)
/// has a sane bail latency.
const SWAP_LIVENESS_GRACE_FLOOR: Duration = Duration::from_millis(50);

/// The render callback's liveness, sampled off-thread by a swap: `callbacks` is a monotonic
/// per-callback counter, `callback_period` the most recent block's duration (frames / sample rate).
/// A publisher with no live render thread reports `None` (see
/// [`RenderConfigPublisher::render_liveness`]).
#[derive(Clone, Copy)]
pub struct RenderLiveness {
    /// Monotonic count of render callbacks so far — advances iff the callback is running.
    pub callbacks: u64,
    /// The most recent callback period, used to size the liveness grace to the device's block rate.
    pub callback_period: Duration,
}

/// The liveness grace for a poll bounded by `hard`: [`SWAP_LIVENESS_GRACE_PERIODS`] callback periods,
/// clamped to `[SWAP_LIVENESS_GRACE_FLOOR, hard]`. Clamping to `hard` guarantees the grace never
/// exceeds the pre-existing bound, so a device whose period is so long the grace saturates behaves
/// exactly as the old fixed timeout did — the fix never cuts a live device off *earlier* than before.
/// A publisher reporting no period (headless) gets the floor.
fn liveness_grace(period: Option<Duration>, hard: Duration) -> Duration {
    let floor = SWAP_LIVENESS_GRACE_FLOOR.min(hard);
    match period {
        Some(p) => (p * SWAP_LIVENESS_GRACE_PERIODS).clamp(floor, hard),
        None => floor,
    }
}

/// Fast-bail gate for a swap's bounded install/reclaim poll.
///
/// Both polls run under the Coordinator lock and, absent this gate, spin to a fixed ~500ms deadline
/// whenever the render side is not consuming — which starves `get_document` and the next swap when
/// audio has genuinely stopped. The gate watches the render callback's heartbeat and gives up once
/// it has been **flat for the liveness grace** ([`liveness_grace`]): every observed tick re-arms the
/// window, so the gate tracks liveness continuously rather than latching on the first tick. This
/// bails promptly whether audio was already stopped at poll start *or* stalls mid-poll, while a live
/// callback — however far apart its blocks — keeps re-arming the window and is bounded only by the
/// full `hard` deadline. A headless publisher reports no heartbeat (`None`): the window never re-arms
/// and the gate bails at the floor grace — there is no render thread to consume.
///
/// This never shortens a *successful* poll: `reclaim`/`install` return the moment the retiree or
/// install slot is free, before the gate is ever consulted. It only bounds the *failure* wait.
pub(crate) struct SwapPollGate {
    /// The last heartbeat count observed; `None` when the publisher drives no live render thread.
    last_beat: Option<u64>,
    /// When the heartbeat last advanced (or poll start) — the window the grace is measured from.
    last_change: Instant,
    /// Bail once the heartbeat has been flat this long (sized to the callback period at start).
    grace: Duration,
    /// The absolute hard deadline (the pre-existing generous bound), honored even while live.
    hard: Instant,
}

impl SwapPollGate {
    /// Start a gate for a poll bounded by `hard`, seeding from the render `liveness` at `now`.
    pub(crate) fn start(liveness: Option<RenderLiveness>, hard: Duration) -> Self {
        Self::start_at(liveness, Instant::now(), hard)
    }

    /// [`start`](Self::start) with an explicit clock, so the gate's logic is unit-testable without
    /// sleeping.
    pub(crate) fn start_at(liveness: Option<RenderLiveness>, now: Instant, hard: Duration) -> Self {
        Self {
            last_beat: liveness.map(|l| l.callbacks),
            last_change: now,
            grace: liveness_grace(liveness.map(|l| l.callback_period), hard),
            hard: now + hard,
        }
    }

    /// Consult after each empty poll with the *current* heartbeat count: `true` means give up.
    pub(crate) fn give_up(&mut self, callbacks: Option<u64>) -> bool {
        self.give_up_at(callbacks, Instant::now())
    }

    /// [`give_up`](Self::give_up) with an explicit clock (unit-test seam).
    pub(crate) fn give_up_at(&mut self, callbacks: Option<u64>, now: Instant) -> bool {
        if let (Some(prev), Some(cur)) = (self.last_beat, callbacks) {
            if cur != prev {
                // A tick: the callback is live right now, so re-arm the flat-window from here.
                self.last_beat = Some(cur);
                self.last_change = now;
            }
        }
        now >= self.hard || now.saturating_duration_since(self.last_change) >= self.grace
    }
}

/// Publish the render-side config for a freshly-installed engine and report any dark-degrade
/// warnings — the **native device seam** of the mailbox swap.
///
/// After [`Coordinator::swap_document`](reuben_api::engine::Coordinator::swap_document) commits,
/// this rebuilds the device **output map**
/// off-thread against the *retained* device channel count (streams are fixed at `play` start)
/// and ships it across the render mailbox to the RT callback, so the callback
/// installs Engine + map together. It also decides the **input dark-degrade**: a swapped-in
/// engine that binds input channels no open stream provides degrades to silence, and this returns
/// the loud swap-report warning (know-and-say) — not an error, not a crash.
///
/// `play` wires the production implementation (the native map mailbox, in `audio.rs`); a headless
/// test uses [`HeadlessRenderConfig`], which builds no map (there is no device) but still computes
/// the dark-degrade warning from the retained geometry, so the swap **logic** — including the
/// warning — is exercised with no cpal device.
///
/// `Send + Sync` because a connection-handler thread calls it (under the Coordinator lock).
pub trait RenderConfigPublisher: Send + Sync {
    /// Publish the render config for the just-installed engine with `logical` output channels and
    /// `input_channels` input channels, and return any dark-degrade warnings to fold into the
    /// [`SwapReport`](reuben_api::engine::SwapReport).
    fn publish(&self, logical: usize, input_channels: usize) -> Vec<Diag>;

    /// The render callback's [`RenderLiveness`] (callback counter + period), or `None` if this
    /// publisher drives no live render thread. A swap's `SwapPollGate` samples it to tell a running
    /// device from a stopped one and bail early instead of holding the Coordinator lock to the full
    /// `SWAP_RECLAIM_TIMEOUT`.
    ///
    /// **Deliberately has no default.** The gate's correctness for a real device hinges on this being
    /// wired, so every publisher must decide rather than silently inherit `None`. The invariant: a
    /// publisher backing a **live render callback MUST return `Some`** with a counter that advances
    /// once per callback (and that callback's period). Returning `None` — or a `Some` whose counter
    /// never moves — marks the device as *stopped*, so the swap bails at the liveness grace; that is
    /// correct only for a publisher with no live callback (e.g. [`HeadlessRenderConfig`]).
    fn render_liveness(&self) -> Option<RenderLiveness>;
}

/// The default [`RenderConfigPublisher`] for a headless [`NativeHost`]: no
/// device, so no output map is built or shipped, but the **input dark-degrade** warning is still
/// computed from the retained input-stream geometry — the device-independent half the integration
/// and unit tests drive. `opened_input_channels` is how many logical input channels the input
/// stream that opened at `play` start provides (`0` = output-only, no input stream). Production
/// overrides this with the real native map publisher.
pub struct HeadlessRenderConfig {
    /// Logical input channels the play-start input stream provides; `0` for an output-only stream.
    pub opened_input_channels: usize,
}

impl RenderConfigPublisher for HeadlessRenderConfig {
    fn publish(&self, _logical: usize, input_channels: usize) -> Vec<Diag> {
        dark_degrade_warning(input_channels, self.opened_input_channels)
    }

    /// No device, no render callback: report no liveness, so a swap's reclaim bails at the floor
    /// grace instead of spinning the full timeout (the `FakeCallback` tests drive real consumption,
    /// so their reclaims complete before the gate is ever consulted).
    fn render_liveness(&self) -> Option<RenderLiveness> {
        None
    }
}

/// Whether an input stream opened `opened_input_channels` wide feeds an engine that binds
/// `input_channels`. The render callback feeds input only on an exact match; on any other, the
/// engine's input pipes read silence, and a stream with nothing to feed runs unread. Only a
/// restart, which opens the stream for the instrument it starts on, changes the width.
pub fn input_stream_fits(input_channels: usize, opened_input_channels: usize) -> bool {
    input_channels == opened_input_channels
}

/// The input dark-degrade warning, shared by the headless and production publishers so the two
/// can't drift. It fires whenever [`input_stream_fits`] does not: a swapped-in engine binding
/// `input_channels` logical input channels while the open input stream provides
/// `opened_input_channels` is fed no input at all, so every bound input pipe reads silence; and an
/// engine binding none leaves the stream running unread. A warning, never an error: the engine
/// stays alive, and only a restart opens a stream of another width.
pub(crate) fn dark_degrade_warning(
    input_channels: usize,
    opened_input_channels: usize,
) -> Vec<Diag> {
    if input_stream_fits(input_channels, opened_input_channels) {
        return Vec::new();
    }
    let message = if input_channels == 0 {
        format!(
            "swapped-in instrument binds no input channels, but the {opened_input_channels}-channel \
             input stream opened at `play` start keeps running, unread; restart `play` to close it"
        )
    } else {
        format!(
            "swapped-in instrument binds {input_channels} input channel(s) but the open input \
             stream provides {opened_input_channels} (fixed at `play` start); its input pipes \
             dark-degrade to silence. The engine stays alive and silent; restart `play` with a \
             matching input-binding instrument to capture live input."
        )
    };
    vec![Diag {
        node: None,
        port: None,
        message,
    }]
}

/// The native seams the window's engine verbs are served over: the control ingress, the counters,
/// the device map and the deferred free.
///
/// Cheap to hold behind an `Arc` and shared by every connection handler. The connection threads
/// share one control ring pair into the render callback, behind a lock only they take; `play`'s
/// UDP decode thread has a pair of its own, and both converge at the callback's `queue_osc`.
pub struct NativeHost {
    diagnostics: Arc<Diagnostics>,
    meters: Meters,
    probes: Arc<Mutex<Probes>>,
    control: Mutex<ControlProducer>,
    render_config: Arc<dyn RenderConfigPublisher>,
    hold: Option<SwapHold>,
    library_root: Option<PathBuf>,
    /// The directories [`offer_dir`](Self::offer_dir) added, absolute and normalized, each once.
    extra_dirs: Mutex<Vec<PathBuf>>,
    /// Where `send_midi` injects; `None` refuses it.
    midi: Option<SharedRouter>,
    /// Counts the MIDI map writes made through the channel.
    midi_map_generation: Arc<AtomicU64>,
}

/// What decides whether a client's open is held for the host's user rather than installed:
/// [`EngineHost::hold_swap`], as a value an [`EngineConfig`](crate::engine::EngineConfig) can
/// carry. Called with the name the client opened, made absolute against the working directory
/// with `..` folded; `true` holds it. Runs under the Coordinator lock, so it must not wait on the
/// user.
#[derive(Clone)]
pub struct SwapHold(Arc<dyn Fn(&str) -> bool + Send + Sync>);

impl SwapHold {
    pub fn new(hold: impl Fn(&str) -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(hold))
    }
}

impl std::fmt::Debug for SwapHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SwapHold")
    }
}

impl NativeHost {
    /// The seam `play` (or a test) fills: the live [`Diagnostics`] surface the callback feeds, and
    /// the control ingress it drains.
    ///
    /// Built with the headless [`HeadlessRenderConfig`] (output-only: any input-binding swap
    /// dark-degrades) — production wires the real native map publisher with
    /// [`with_render_config`](Self::with_render_config). That one *is* a builder step because it
    /// has a working default; the control ingress has none, so it is a parameter.
    pub fn new(diagnostics: Arc<Diagnostics>, control: ControlProducer) -> Self {
        Self::with_probe_lease(diagnostics, control, LEASE, LEASE_CHECK)
    }

    /// [`new`](Self::new) with the probe lease and the period its expiry thread checks at, so a
    /// test need not wait the production 5 s and 1 s.
    pub(crate) fn with_probe_lease(
        diagnostics: Arc<Diagnostics>,
        control: ControlProducer,
        lease: Duration,
        check: Duration,
    ) -> Self {
        let probes = Arc::new(Mutex::new(Probes::with_lease(lease)));
        spawn_lease_thread(Arc::downgrade(&probes), check);
        Self {
            diagnostics,
            meters: Meters::none(),
            probes,
            control: Mutex::new(control),
            render_config: Arc::new(HeadlessRenderConfig {
                opened_input_channels: 0,
            }),
            hold: None,
            library_root: None,
            extra_dirs: Mutex::new(Vec::new()),
            midi: None,
            midi_map_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// What moves each time the open instrument's MIDI map is written through the channel: the
    /// route watch rebuilds on it, so routing follows such a write without watching the file.
    pub(crate) fn midi_map_generation(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.midi_map_generation)
    }

    /// Route `send_midi` through `router`, the one `play`'s MIDI devices share.
    pub fn with_midi_router(mut self, router: SharedRouter) -> Self {
        self.midi = Some(router);
        self
    }

    /// Answer `GetMeters` from the banks the audio callbacks meter. Without it the host has no
    /// device, and answers empty arrays.
    pub fn with_meters(mut self, meters: Meters) -> Self {
        self.meters = meters;
        self
    }

    /// The directories a client may name: the library root, every [offered](Self::offer_dir)
    /// directory and the open instrument's directory, absolute and normalized, each once.
    fn offered_dirs(&self, open: Option<&str>) -> Vec<PathBuf> {
        let open_dir = open
            .and_then(|source| lexical_absolute(Path::new(source)))
            .and_then(|source| source.parent().map(Path::to_path_buf));
        let extra = self.extra_dirs().clone();
        let mut dirs = Vec::new();
        for dir in self
            .library_root
            .iter()
            .filter_map(|root| lexical_absolute(root))
            .chain(extra)
            .chain(open_dir)
        {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        dirs
    }

    /// [`offered_dirs`](Self::offered_dirs), each with whether [`offered`](EngineHost::offered)
    /// lists beneath it. The library root's and the open instrument's directory's subdirectories
    /// list, wherever else that directory was named from: "Open file…" offers the file's own
    /// directory before it opens the file, and its voices and samples must still list. Any other
    /// offered directory lists only its own files, since one may be as wide as a home directory.
    fn walked_dirs(&self, open: Option<&str>) -> Vec<(PathBuf, bool)> {
        let root = (self.library_root.as_deref()).and_then(lexical_absolute);
        let open_dir = open
            .and_then(|source| lexical_absolute(Path::new(source)))
            .and_then(|source| source.parent().map(Path::to_path_buf));
        self.offered_dirs(open)
            .into_iter()
            .map(|dir| {
                let deep = Some(&dir) == root.as_ref() || Some(&dir) == open_dir.as_ref();
                (dir, deep)
            })
            .collect()
    }

    /// Offer `dir` from now on, beside the library root and the open instrument's directory, in
    /// [`offered`](EngineHost::offered), [`within`](EngineHost::within),
    /// [`library`](EngineHost::library) and [`creatable`](EngineHost::creatable). A directory that
    /// cannot be made absolute is not offered.
    pub fn offer_dir(&self, dir: &Path) {
        let Some(dir) = lexical_absolute(dir) else {
            return;
        };
        let mut extra = self.extra_dirs();
        if !extra.contains(&dir) {
            extra.push(dir);
        }
    }

    /// The offered directories. Every write leaves the list whole, so a panic while it was held
    /// leaves it fit to keep serving.
    fn extra_dirs(&self) -> std::sync::MutexGuard<'_, Vec<PathBuf>> {
        self.extra_dirs.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The library root [`offered`](EngineHost::offered) lists from, besides the open instrument's
    /// own directory: the one the session's resolver falls back to.
    pub fn with_library_root(mut self, root: Option<PathBuf>) -> Self {
        self.library_root = root;
        self
    }

    /// The probe set, which only engine-channel threads lock. A panic while it was held leaves
    /// it consistent enough to keep serving.
    fn probes(&self) -> std::sync::MutexGuard<'_, Probes> {
        self.probes.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Hold the client opens `hold` says to. Without it none is held, as `reuben play` wants.
    pub fn with_swap_hold(mut self, hold: Option<SwapHold>) -> Self {
        self.hold = hold;
        self
    }

    /// Wire the production render-config seam (`play`'s native map publisher). Without this a swap
    /// updates the installed document + reclaims the retiree but ships no output map and reports
    /// the headless dark-degrade warning (the test seam).
    pub fn with_render_config(mut self, render_config: Arc<dyn RenderConfigPublisher>) -> Self {
        self.render_config = render_config;
        self
    }
}

/// `path` made absolute against the working directory with `.` and `..` resolved by text alone, so
/// a name cannot climb out of a directory it starts under. `None` when it cannot be made absolute.
pub fn lexical_absolute(path: &Path) -> Option<PathBuf> {
    let absolute = std::path::absolute(path).ok()?;
    let mut normal = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::ParentDir => {
                normal.pop();
            }
            std::path::Component::CurDir => {}
            other => normal.push(other),
        }
    }
    Some(normal)
}

/// Add every `*.{extension}` file in `dir` to `found`, and, when `deep`, in its subdirectories. An
/// unreadable directory contributes nothing.
fn collect_with_extension(
    dir: &Path,
    extension: &str,
    deep: bool,
    found: &mut std::collections::BTreeSet<String>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if deep {
                collect_with_extension(&path, extension, deep, found);
            }
        } else if path.extension().is_some_and(|e| e == extension) {
            found.insert(path.display().to_string());
        }
    }
}

impl EngineHost for NativeHost {
    /// Absolute, because the session's `FsResolver` joins a relative source onto the launch
    /// instrument's directory while a client means `path` from the working directory: only an
    /// absolute spelling names the same file to both. `..` folds by text, so the file read is the
    /// one the store knows by that name even where a symlinked directory precedes the `..`.
    /// Normalized as [`library`](EngineHost::library) and [`offered`](EngineHost::offered) spell
    /// their names, so a client that opened one of them hears it back by the name it opened.
    fn document_origin(&self, path: &str) -> String {
        match lexical_absolute(Path::new(path)) {
            Some(absolute) => absolute.display().to_string(),
            // Fails for an empty path, or a relative one with no working directory. Neither is
            // within what the host offers, so the open is refused before this value is read.
            None => path.to_string(),
        }
    }

    /// Every file with extension `what` names, under the library root and under the directory of
    /// the open instrument, recursively, and directly in each offered directory: absolute, sorted,
    /// each once. Nothing outside those.
    fn offered(&self, what: ListWhat, open: Option<&str>) -> Vec<String> {
        let extension = match what {
            ListWhat::Instruments => "json",
            ListWhat::Samples => "wav",
        };
        let mut found = std::collections::BTreeSet::new();
        for (dir, deep) in self.walked_dirs(open) {
            collect_with_extension(&dir, extension, deep, &mut found);
        }
        found.into_iter().collect()
    }

    fn surface_written(&self, surface: SurfaceKind) {
        if surface == SurfaceKind::Midi {
            // Bumped after the write committed, so a watch that sees the new count reads the new
            // map.
            self.midi_map_generation.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// True iff `name`, made absolute and lexically normalized (`..` resolved), lies under the
    /// library root or the open instrument's directory. The file need not exist.
    fn within(&self, name: &str, open: Option<&str>) -> bool {
        let Some(path) = lexical_absolute(Path::new(name)) else {
            return false;
        };
        self.offered_dirs(open)
            .iter()
            .any(|dir| path.starts_with(dir))
    }

    /// Every `.json` file directly in an offered directory, not in its subdirectories: absolute,
    /// sorted, each once.
    fn library(&self, open: Option<&str>) -> Vec<String> {
        let mut found = std::collections::BTreeSet::new();
        for dir in self.offered_dirs(open) {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                // `Path::is_file` follows a symlink, which `DirEntry::file_type` does not.
                if path.is_file() && path.extension().is_some_and(|e| e == "json") {
                    found.insert(path.display().to_string());
                }
            }
        }
        found.into_iter().collect()
    }

    /// The offered directories, absolute.
    fn creatable(&self, open: Option<&str>) -> Vec<String> {
        self.offered_dirs(open)
            .iter()
            .map(|dir| dir.display().to_string())
            .collect()
    }

    /// **One push onto the ring, not one per message.** That single push is what makes the
    /// documented atomicity true rather than aspirational: concurrent handler threads cannot
    /// interleave message-by-message into each other's gestures, and the callback cannot apply half
    /// a gesture to one block and half to the next.
    ///
    /// A batch the full ring refuses is dropped and counted, and the `send` still answers `Ok`.
    fn deliver_control(&self, messages: Vec<PreparedMessage>) -> Result<(), IngressClosed> {
        let mut control = self.control.lock().unwrap_or_else(|e| e.into_inner());
        if control.is_closed() {
            return Err(IngressClosed);
        }
        let mut batch = control.batch();
        batch.extend(messages.into_iter().map(|message| OscIn {
            address: message.address,
            args: message.args,
            against: message.against,
            dropped: false,
        }));
        let _ = control.push(batch);
        Ok(())
    }

    /// Refuses an address and args no device could send, naming them, so a typo in an injected
    /// message is an error rather than a silent `midi_unmatched`.
    fn deliver_midi(&self, address: &str, args: Vec<Arg>) -> Result<(), String> {
        let Some(router) = &self.midi else {
            return Err("this engine reads no MIDI".to_string());
        };
        let translated = parse_translated(address, &args).ok_or_else(|| {
            let shown: Vec<String> = args
                .iter()
                .map(|arg| match arg {
                    Arg::F32(v) => v.to_string(),
                    Arg::I32(v) => v.to_string(),
                    other => format!("{other:?}"),
                })
                .collect();
            format!(
                "`{address} {}` is not a MIDI message as a device sends it. Channels are 1 to 16; \
                 send /midi/<ch>/note <pitch 0 to 127> <velocity 0 to 1>, \
                 /midi/<ch>/cc/<0 to 127> <0 to 1>, /midi/<ch>/bend <-1 to 1>, \
                 /midi/<ch>/pressure <0 to 1> or /midi/<ch>/program <0 to 127>",
                shown.join(" ")
            )
        })?;
        router
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .translated(&translated);
        Ok(())
    }

    fn take_dropped_sends(&self) -> Vec<String> {
        self.control
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take_dropped()
    }

    /// **RT-safety:** [`Diagnostics::snapshot`] is `Relaxed` atomic loads into an owned copy, so
    /// this query thread never forces the audio callback to synchronize.
    fn diagnostics(&self) -> DiagnosticsReport {
        diagnostics_report(&self.diagnostics.snapshot())
    }

    /// Relaxed loads into an owned report, like [`diagnostics`](Self::diagnostics).
    fn meters(&self) -> MetersReport {
        let mut report = self.meters.report();
        report.probes = self.probes().report(Instant::now());
        report
    }

    fn set_probes(&self, paths: Vec<Vec<String>>) -> Result<Vec<Vec<String>>, String> {
        Ok(self.probes().set(paths, Instant::now()))
    }

    fn install_probes(&self, slots: Arc<ProbeSlots>) {
        self.probes().install(slots, Instant::now());
    }

    fn publish_render_config(&self, logical: usize, input_channels: usize) -> Vec<Diag> {
        self.render_config.publish(logical, input_channels)
    }

    fn hold_swap(&self, path: &str) -> bool {
        self.hold.as_ref().is_some_and(|hold| (hold.0)(path))
    }

    /// Polls the retire slot with a 1ms back-off (the window is OS-free; the clock is ours),
    /// bounded by a `SwapPollGate`: at most `SWAP_RECLAIM_TIMEOUT` while the callback keeps
    /// ticking, but only the liveness grace once the render callback has stopped ticking. With a
    /// live callback the retiree comes home in one master-gain ramp (~20ms), so the bound only
    /// bites when audio has genuinely stopped.
    fn retire_poll(&self) -> Box<dyn FnMut() -> bool + Send + '_> {
        let liveness = || self.render_config.render_liveness();
        let mut gate = SwapPollGate::start(liveness(), SWAP_RECLAIM_TIMEOUT);
        Box::new(move || {
            std::thread::sleep(Duration::from_millis(1));
            gate.give_up(liveness().map(|l| l.callbacks))
        })
    }
}

/// Map a diagnostics [`Snapshot`] (this crate's counter surface) to the window's wire
/// [`DiagnosticsReport`].
///
/// The two structs are duplicated across the crate boundary with no shared definition, so they
/// could silently drift. The **exhaustive destructure** below is the compile-time coupling that
/// prevents it: add a counter to [`Snapshot`] and this `let Snapshot { .. }` stops compiling
/// (non-exhaustive, no `..`); add a field to [`DiagnosticsReport`] and the struct literal stops
/// compiling (missing field). Either drift is a build break here, not a runtime surprise. The
/// behavioral half — that each counter maps to the *right* field — is
/// `tests::diagnostics_report_maps_every_counter_field_for_field`.
pub fn diagnostics_report(snapshot: &Snapshot) -> DiagnosticsReport {
    let Snapshot {
        output_xruns,
        input_ring_underruns,
        input_ring_overruns,
        input_ring_producer_drops,
        control_batches_dropped_full,
        wired_writes_dropped,
        retire_queue_dropped_full,
        unprepared_writes_dropped,
        stale_sends_dropped,
        pattern_queries_truncated,
        pattern_atoms_skipped,
        pattern_notes_evicted,
        midi_received,
        midi_unsupported,
        midi_routed,
        midi_unmatched,
        midi_devices_lost,
    } = *snapshot;
    DiagnosticsReport {
        output_xruns,
        input_ring_underruns,
        input_ring_overruns,
        input_ring_producer_drops,
        control_batches_dropped_full,
        wired_writes_dropped,
        retire_queue_dropped_full,
        unprepared_writes_dropped,
        stale_sends_dropped,
        pattern_queries_truncated,
        pattern_atoms_skipped,
        pattern_notes_evicted,
        midi_received,
        midi_unsupported,
        midi_routed,
        midi_unmatched,
        midi_devices_lost,
    }
}

/// A running engine-channel server: a std thread accepting connections, one handler thread
/// per connection, all joined on [`shutdown`](Self::shutdown) (or `Drop`). Holds no audio state
/// — it only reads the shared [`ChannelState`] — so it starts and stops independently of the
/// audio device, which is what lets it be exercised end-to-end in tests.
pub struct ChannelServer {
    local_addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl ChannelServer {
    /// Bind a loopback TCP listener and start serving. Pass `127.0.0.1:0` to let the OS assign
    /// an ephemeral port (read it back with [`local_addr`](Self::local_addr)); this is how tests
    /// avoid port collisions. A non-loopback address is refused: that takes
    /// [`bind_with_token`](Self::bind_with_token).
    pub fn bind<A: ToSocketAddrs>(addr: A, state: ChannelState) -> std::io::Result<Self> {
        Self::bind_with_token(addr, state, None)
    }

    /// [`bind`](Self::bind), serving peers on other machines when they open with `token`'s
    /// credential line. Loopback peers need none. A non-loopback address with no token is refused
    /// with [`InvalidInput`](std::io::ErrorKind::InvalidInput), judged by the address actually
    /// bound, so a name that resolves beyond loopback is caught too.
    pub fn bind_with_token<A: ToSocketAddrs>(
        addr: A,
        state: ChannelState,
        token: Option<Token>,
    ) -> std::io::Result<Self> {
        Self::bind_admitting(addr, state, token, ADMIT_WITHIN)
    }

    /// [`bind_with_token`](Self::bind_with_token), giving a peer elsewhere `admit_within` to send
    /// its first line instead of [`ADMIT_WITHIN`]: the seam that lets a test watch the deadline
    /// close a connection without waiting out the real one.
    pub(crate) fn bind_admitting<A: ToSocketAddrs>(
        addr: A,
        state: ChannelState,
        token: Option<Token>,
        admit_within: Duration,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        if token.is_none() && !listener.local_addr()?.ip().to_canonical().is_loopback() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "serving the engine channel beyond loopback takes a token",
            ));
        }
        let token = token.map(Arc::new);
        // Non-blocking so the accept loop can poll its shutdown flag with no client connected.
        listener.set_nonblocking(true)?;
        let local_addr = listener.local_addr()?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let accept = {
            let shutdown = Arc::clone(&shutdown);
            std::thread::Builder::new()
                .name("engine-accept".to_string())
                .spawn(move || accept_loop(listener, state, token, admit_within, shutdown))
                .expect("spawn engine-accept thread")
        };
        Ok(Self {
            local_addr,
            shutdown,
            accept: Some(accept),
        })
    }

    /// The bound local address — the ephemeral port the OS assigned when binding to `:0`.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stop accepting, wake and join every connection handler, then join the accept thread.
    /// Returns only once all engine-channel threads have exited — the clean, joinable stop that
    /// replaces `play`'s park-forever loop. Idempotent with `Drop`.
    pub fn shutdown(mut self) {
        self.signal_and_join();
    }

    fn signal_and_join(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

impl Drop for ChannelServer {
    fn drop(&mut self) {
        self.signal_and_join();
    }
}

/// Wake a handler's blocked read (a socket `shutdown` on the clone unblocks its `read_line`) and
/// join it. Best-effort and idempotent: waking an already-finished handler is a harmless no-op and
/// joining it returns at once, so this serves both the mid-run reap (already finished) and the
/// shutdown drain (still live) — one shape, so the two sites can't drift.
fn wake_and_join(handle: JoinHandle<()>, wake: Option<TcpStream>) {
    if let Some(wake) = wake {
        let _ = wake.shutdown(Shutdown::Both);
    }
    let _ = handle.join();
}

/// The accept thread: poll the non-blocking listener until the shutdown flag flips, spawning a
/// blocking handler per connection. On shutdown, wake each live handler (a socket `shutdown`
/// unblocks its `read_line`) and join it, so no idle client keeps the process alive — the exact
/// hang `play`'s old `loop { thread::park() }` had, removed.
fn accept_loop(
    listener: TcpListener,
    state: ChannelState,
    token: Option<Arc<Token>>,
    admit_within: Duration,
    shutdown: Arc<AtomicBool>,
) {
    // (join handle, a clone of the socket used only to wake the handler at shutdown).
    let mut handlers: Vec<(JoinHandle<()>, Option<TcpStream>)> = Vec::new();
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, peer)) => {
                // The accepted socket inherits the listener's non-blocking mode on some
                // platforms; the handler wants blocking reads (clean NDJSON framing — a
                // partial-line timeout would corrupt the next request), so force it.
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_nodelay(true);
                let wake = stream.try_clone().ok();
                let handler_state = state.clone();
                let handler_shutdown = Arc::clone(&shutdown);
                let admission = Admission::new(peer.ip(), token.clone(), admit_within);
                let spawned = std::thread::Builder::new()
                    .name("engine-conn".to_string())
                    .spawn(move || {
                        handle_connection(stream, handler_state, admission, handler_shutdown)
                    });
                // Out of threads: this connection is dropped, and the channel keeps serving.
                if let Ok(handle) = spawned {
                    handlers.push((handle, wake));
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL);
            }
            // A transient accept error is not fatal to the channel; back off and re-check the
            // shutdown flag on the next turn rather than spin.
            Err(_) => std::thread::sleep(ACCEPT_POLL),
        }

        // Reap completed handlers during normal operation so this vector stays bounded.
        let mut i = 0;
        while i < handlers.len() {
            if handlers[i].0.is_finished() {
                let (handle, wake) = handlers.remove(i);
                wake_and_join(handle, wake);
            } else {
                i += 1;
            }
        }
    }
    for (handle, wake) in handlers {
        wake_and_join(handle, wake);
    }
}

/// Serve one connection: read one [`Request`] per line, write one [`Response`] per line, in
/// order, until the client closes or shutdown wakes the blocked read. Blocking
/// reads keep the framing exact; a blank line is framing noise, not a request, so it draws no
/// response. A [`Request::Subscribe`] turns the connection into an event stream for the rest of
/// its life. The first line is [`admit`]ted first: a credential is read and dropped, and a refusal
/// is answered and closes the connection.
fn handle_connection(
    stream: TcpStream,
    state: ChannelState,
    admission: Admission,
    shutdown: Arc<AtomicBool>,
) {
    let mut admitted = false;
    // Time-bound each read so the handler wakes to observe `shutdown` itself, rather than
    // depending on the accept thread's `shutdown(Shutdown::Both)` to unblock it — a wake that
    // does not reach a blocked recv on Windows (see [`READ_POLL`]). A request that spans reads
    // accumulates in `line` across the loop until its terminating newline.
    let _ = stream.set_read_timeout(Some(READ_POLL));
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        // Can't split the socket into independent read/write halves; nothing to serve on.
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        if !admitted && admission.deadline.is_some_and(|by| Instant::now() >= by) {
            let refused = Response::Error {
                message: TOKEN_REQUIRED.to_string(),
            };
            refuse(&mut writer, &mut reader, &refused);
            break;
        }
        match read_ndjson_line(&mut reader, &mut line) {
            // EOF: client closed, or a shutdown `Shutdown::Both` woke us.
            Ok(0) => break,
            // A complete line (`read_line` returns on the newline). Dispatch, then reset the buffer.
            Ok(_) => {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                if !line.trim().is_empty() && !admitted {
                    admitted = true;
                    match admit(admission.token.as_deref(), admission.peer, &line) {
                        Admit::Credential => {
                            line.clear();
                            continue;
                        }
                        Admit::Refused(response) => {
                            refuse(&mut writer, &mut reader, &response);
                            break;
                        }
                        Admit::Request => {}
                    }
                }
                if !line.trim().is_empty() {
                    let response = match Request::from_ndjson(&line) {
                        Ok(Request::Subscribe) => {
                            // Subscribed before the reply, so no change made after the client
                            // reads it is missed.
                            let events = state.subscription();
                            if write_line(&mut writer, &Response::Subscribed.to_ndjson()) {
                                stream_events(&mut writer, &reader, &events, &shutdown);
                            }
                            break;
                        }
                        Ok(request) => dispatch_request(&state, request),
                        Err(_) => dispatch(&state, &line),
                    };
                    if !write_line(&mut writer, &response.to_ndjson()) {
                        break;
                    }
                }
                line.clear();
            }
            // The read timeout fired with no full line yet: loop to re-check `shutdown`. The
            // partial bytes stay in `line` and the next read resumes the request, unless they end
            // inside a UTF-8 character: `read_line` drops those, and the orphaned rest of the
            // character fails the next read as InvalidData, below. This is the poll that makes
            // shutdown prompt.
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            // A line over the cap leaves the rest of it unread, and a line that is not UTF-8 may
            // be one whose start a timeout dropped; either way the stream is no longer known to be
            // at a line boundary, so say why, then close rather than guess where the next request
            // starts. The reply is best-effort: closing with bytes still unread resets the
            // connection, which can discard the reply before a client still writing reads it.
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                let response = Response::Error {
                    message: format!("unreadable request: {e}"),
                };
                let _ = writer.write_all(response.to_ndjson().as_bytes());
                let _ = writer.flush();
                break;
            }
            // A real read error ends the connection.
            Err(_) => break,
        }
    }
}

/// Who is on the other end of a connection, and what the server asks of a peer elsewhere.
struct Admission {
    peer: IpAddr,
    token: Option<Arc<Token>>,
    /// When a peer that must present the token is closed for not having sent a first line, so an
    /// idle stranger cannot hold a handler thread.
    deadline: Option<Instant>,
}

impl Admission {
    fn new(peer: IpAddr, token: Option<Arc<Token>>, admit_within: Duration) -> Self {
        let must_present = token.is_some() && !peer.to_canonical().is_loopback();
        Self {
            peer,
            token,
            deadline: must_present.then(|| Instant::now() + admit_within),
        }
    }
}

/// Answer a connection being refused, then close it so the answer arrives. A client writes its
/// credential and its request together, so the request is usually still unread here; closing over
/// unread bytes resets the connection, and on Windows the reset discards the answer before the
/// client reads it. So the write half is shut first and what the peer sent is read off until it
/// closes, for at most [`REFUSAL_LINGER`].
fn refuse(writer: &mut TcpStream, reader: &mut BufReader<TcpStream>, response: &Response) {
    if !write_line(writer, &response.to_ndjson()) || writer.shutdown(Shutdown::Write).is_err() {
        return;
    }
    let until = Instant::now() + REFUSAL_LINGER;
    let mut sink = [0u8; 1024];
    while Instant::now() < until {
        match reader.get_mut().read(&mut sink) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }
}

/// How long a refused connection is read off before it is closed regardless. A client closes as
/// soon as it has read its answer, so this only bounds a peer that keeps the connection open.
const REFUSAL_LINGER: Duration = Duration::from_secs(1);

/// How long a peer on another machine has to send its first line. A client sends its credential
/// in the same write as its first request, so any real one is far inside this.
const ADMIT_WITHIN: Duration = Duration::from_secs(5);

/// Write and flush one line; `false` when the connection is gone.
fn write_line(writer: &mut TcpStream, line: &str) -> bool {
    writer.write_all(line.as_bytes()).is_ok() && writer.flush().is_ok()
}

/// How long one event write may block before the subscriber is dropped. An event line is a few
/// hundred bytes and the kernel's send buffer holds many of them, so a write only blocks once a
/// subscriber has stopped reading for a whole buffer's worth; ten seconds is far past any stall a
/// LAN or tunnelled link recovers from, and short enough that a dead peer does not hold its
/// handler for the OS's own keepalive timeout.
const EVENT_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long each check for the subscriber having closed waits on the socket. Short, because it is
/// paid once per [`ACCEPT_POLL`] and only ever ends early when the peer sent something.
const PEER_CHECK: Duration = Duration::from_millis(1);

/// Write each event as it is published until the client goes away or the server shuts down. The
/// wait is bounded so the shutdown flag is seen within [`ACCEPT_POLL`] even when nothing changes.
///
/// Between waits the read half is checked: end of stream means the subscriber is gone, and any
/// byte at all is a request on a connection that answers none, so either ends the stream. Ending
/// drops `events`, which unsubscribes it at once rather than at the next publish.
fn stream_events(
    writer: &mut TcpStream,
    reader: &BufReader<TcpStream>,
    events: &Subscription,
    shutdown: &AtomicBool,
) {
    // Bytes the client sent after `subscribe` and the line reader already buffered.
    if !reader.buffer().is_empty() {
        return;
    }
    let peer = reader.get_ref();
    if writer.set_write_timeout(Some(EVENT_WRITE_TIMEOUT)).is_err()
        || peer.set_read_timeout(Some(PEER_CHECK)).is_err()
    {
        return;
    }
    let mut probe = [0u8; 1];
    while !shutdown.load(Ordering::SeqCst) {
        match events.recv_timeout(ACCEPT_POLL) {
            Ok(event) => {
                if !write_line(writer, &event.to_ndjson()) {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        match peer.peek(&mut probe) {
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            // Closed, sent a request, or failed.
            Ok(_) | Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{ring_pair, ControlRing};
    use crate::test_support::{no_store, served, FakeCallback};
    use reuben_api::engine::{ControlArg, ControlMessage, Request, Response, MAX_SEND_BATCH};
    use reuben_api::find_surface;
    use reuben_api::render::{Arg, AudioConfig};

    fn cfg() -> AudioConfig {
        AudioConfig::new(48_000.0, 128)
    }

    /// A minimal output-only rig to swap *from*: one oscillator through a master output.
    const BASE_DOC: &str = r#"{"format_version":3,"instrument":"t",
        "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
        "nodes":[{"type":"oscillator","address":"/osc"}]}"#;

    /// A held envelope whose CV is the master output (rings at sustain) — a swap to the identical
    /// document keeps `/env` + `/out` survivors, so the real diff carries `survived: 2`, the
    /// load-bearing proof the mailbox swap ran rather than a cold restart.
    fn envelope_doc(env_addr: &str) -> String {
        format!(
            r#"{{ "format_version": 3, "instrument": "eg",
                 "interface": {{ "outputs": {{ "out": {{ "from": "/out.audio" }} }} }},
                 "nodes": [
                   {{ "type": "envelope", "address": "{env_addr}",
                      "inputs": {{ "gate": 1.0, "attack": 0.5, "decay": 0.01,
                                   "sustain": 0.8, "release": 0.5 }} }},
                   {{ "type": "output", "address": "/out",
                      "inputs": {{ "audio": {{ "from": "{env_addr}.cv" }} }} }} ] }}"#
        )
    }

    /// A minimal instrument that binds logical input channel 0: the swapped-in engine
    /// wants live input, which an output-only stream can't provide — the dark-degrade case.
    const MIC_PASSTHRU: &str = r#"{ "format_version": 3, "instrument": "mic-passthru",
        "interface": {
            "inputs": { "mic": { "type": "f32_buffer", "channel": 0 } },
            "outputs": { "out": { "from": "/mic" } } },
        "nodes": [] }"#;

    /// A document that fails to load — an unknown operator type — so validation rejects it.
    /// A Coordinator-backed [`ChannelState`] with a live [`FakeCallback`] draining its mailbox
    /// and its control ingress, plus the base document's content hash. `opened_input_channels` sets
    /// the headless render config's input-stream geometry (`0` = output-only, so an input-binding
    /// swap dark-degrades).
    ///
    /// The harness is [`crate::test_support::FakeCallback`], shared with the integration tests, so
    /// there is exactly one mirror of the real callback's control drain.
    fn swap_fixture(
        base: &str,
        opened_input_channels: usize,
    ) -> (ChannelState, FakeCallback, String) {
        let staged = no_store();
        let (coordinator, side, _warnings) =
            reuben_api::engine::install_initial(base, staged.clone(), cfg())
                .expect("initial install");
        let base_hash = coordinator.installed_hash();
        let (control_tx, control_rx) = ring_pair(Diagnostics::new());
        let host = NativeHost::new(Diagnostics::new(), control_tx).with_render_config(Arc::new(
            HeadlessRenderConfig {
                opened_input_channels,
            },
        ));
        let state = ChannelState::new(coordinator, Arc::new(host), served(staged));
        (state, FakeCallback::spawn(side, control_rx), base_hash)
    }

    /// A directory the host offers, holding the documents an open names. Removed on drop.
    struct Library(PathBuf);

    impl Drop for Library {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl Library {
        /// `text` written here as `file`, and the open that names it.
        fn open(&self, file: &str, text: &str, expect: Option<String>) -> Request {
            let path = self.0.join(file);
            std::fs::write(&path, text).expect("seed the document");
            Request::Open {
                name: path.display().to_string(),
                expect,
            }
        }
    }

    /// [`swap_fixture`] with `base` playing from a [`Library`] the host offers and the server's
    /// store reads. `case` keeps concurrent tests out of each other's directory.
    fn open_fixture(base: &str, case: &str) -> (ChannelState, FakeCallback, String, Library) {
        let (state, side, control_rx, base_hash, library) = open_parts(base, case);
        (
            state,
            FakeCallback::spawn(side, control_rx),
            base_hash,
            library,
        )
    }

    /// [`open_fixture`] with no render side running yet.
    fn open_parts(
        base: &str,
        case: &str,
    ) -> (
        ChannelState,
        reuben_api::render::RenderSide,
        ControlRing,
        String,
        Library,
    ) {
        let (state, side, control_rx, base_hash, library, _host) =
            open_parts_counting(base, case, Diagnostics::new());
        (state, side, control_rx, base_hash, library)
    }

    /// [`open_parts`], the host counting into `diagnostics` and handed back too.
    fn open_parts_counting(
        base: &str,
        case: &str,
        diagnostics: Arc<Diagnostics>,
    ) -> (
        ChannelState,
        reuben_api::render::RenderSide,
        ControlRing,
        String,
        Library,
        Arc<NativeHost>,
    ) {
        use reuben_api::resources::Resources;
        use reuben_api::staged::Staged;
        use reuben_api::FsResolver;

        let dir = std::env::temp_dir().join(format!("reuben_open_{case}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("case dir");
        let library = Library(dir);
        let top = library.0.join("base.json");
        std::fs::write(&top, base).expect("seed what plays");
        let staged: Staged<Box<dyn Resources + Send + Sync>> =
            Staged::new(Box::new(FsResolver::for_instrument(&top)));
        let (coordinator, side, _warnings) =
            reuben_api::engine::install_initial(base, staged.clone(), cfg())
                .expect("initial install");
        let base_hash = coordinator.installed_hash();
        let (control_tx, control_rx) = ring_pair(Arc::clone(&diagnostics));
        let host = Arc::new(
            NativeHost::new(diagnostics, control_tx)
                .with_library_root(Some(library.0.clone()))
                .with_render_config(Arc::new(HeadlessRenderConfig {
                    opened_input_channels: 0,
                })),
        );
        let state = ChannelState::new(
            coordinator,
            Arc::clone(&host) as Arc<dyn EngineHost>,
            served(staged),
        )
        .with_installed_source(Some(top.display().to_string()));
        (state, side, control_rx, base_hash, library, host)
    }

    #[test]
    fn diagnostics_report_maps_every_counter_field_for_field() {
        // Distinct values per counter so a mis-wire (mapping overruns to underruns, say) is
        // caught — equal values would let a swapped pair pass.
        let d = Diagnostics::new();
        d.record_output_xrun();
        d.record_output_xrun();
        d.record_output_xrun(); // 3 events
        d.record_input_ring_underrun_frames(11);
        d.record_input_ring_overrun_frames(22);
        d.record_input_ring_producer_drop_frames(33);
        for _ in 0..4 {
            d.record_control_batch_dropped_full();
        }
        d.record_wired_writes_dropped(5);
        d.record_wired_writes_dropped(0);
        d.record_unprepared_writes_dropped(6);
        d.record_unprepared_writes_dropped(0);
        d.record_stale_sends_dropped(7);
        d.record_stale_sends_dropped(0);
        d.record_operator_counts(reuben_api::render::OperatorCounts {
            pattern_queries_truncated: 8,
            pattern_atoms_skipped: 9,
            pattern_notes_evicted: 10,
        });
        for (counter, n) in [
            (&d.midi_received, 12),
            (&d.midi_unsupported, 13),
            (&d.midi_routed, 14),
            (&d.midi_unmatched, 15),
            (&d.midi_devices_lost, 16),
        ] {
            for _ in 0..n {
                d.record_midi(counter);
            }
        }
        let report = diagnostics_report(&d.snapshot());
        assert_eq!(report.midi_received, 12);
        assert_eq!(report.midi_unsupported, 13);
        assert_eq!(report.midi_routed, 14);
        assert_eq!(report.midi_unmatched, 15);
        assert_eq!(report.midi_devices_lost, 16);
        assert_eq!(report.output_xruns, 3);
        assert_eq!(report.input_ring_underruns, 11);
        assert_eq!(report.input_ring_overruns, 22);
        assert_eq!(report.input_ring_producer_drops, 33);
        assert_eq!(report.control_batches_dropped_full, 4);
        assert_eq!(report.wired_writes_dropped, 5);
        assert_eq!(report.unprepared_writes_dropped, 6);
        assert_eq!(report.stale_sends_dropped, 7);
        assert_eq!(report.pattern_queries_truncated, 8);
        assert_eq!(report.pattern_atoms_skipped, 9);
        assert_eq!(report.pattern_notes_evicted, 10);
        assert_eq!(
            report.retire_queue_dropped_full, 0,
            "no retire queue watched yet"
        );
    }

    /// The retire overflow is the Coordinator's count: once watched, a snapshot (and so the log
    /// line and the report) carries it.
    #[test]
    fn a_watched_retire_queue_overflow_reaches_the_snapshot() {
        use reuben_api::render::RenderSlot;
        const ECHO: &str = r#"{"format_version":3,"instrument":"echo",
            "interface":{"inputs":{"pat":{"type":"str","default":"seed"}}},
            "nodes":[{"type":"osc_out","address":"/o","inputs":{"in":{"from":"/pat"}}}]}"#;
        let (coordinator, side, _) =
            reuben_api::engine::install_initial(ECHO, no_store(), cfg()).expect("install");
        let d = Diagnostics::new();
        d.watch_retire_queue(coordinator.retired());
        let mut slot = RenderSlot::new(side);
        let mut out = vec![0.0f32; 128 * slot.channels()];
        // Nothing drains between these sends, as for OSC over UDP. Each raw string is retired
        // once, from the pending-control queue, so twice the capacity overflows it.
        for i in 0..2 * reuben_api::engine::RETIRE_QUEUE_CAPACITY {
            slot.queue_osc("/pat/in", &[Arg::Str(format!("pattern {i}").into())]);
            slot.fill(&mut out);
            slot.drain_outbound().for_each(drop);
        }
        let counted = d.snapshot().retire_queue_dropped_full;
        assert!(counted > 0);
        assert_eq!(counted, coordinator.retired().dropped_full());
        assert_eq!(
            diagnostics_report(&d.snapshot()).retire_queue_dropped_full,
            counted
        );
    }

    #[test]
    fn offered_lists_only_library_and_open_dir() {
        let base = std::env::temp_dir().join(format!("reuben-offered-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for dir in ["lib/nested", "open", "elsewhere"] {
            std::fs::create_dir_all(base.join(dir)).unwrap();
        }
        for file in [
            "lib/a.json",
            "lib/s.wav",
            "lib/nested/n.json",
            "open/b.json",
            "elsewhere/c.json",
            "elsewhere/d.wav",
        ] {
            std::fs::write(base.join(file), "").unwrap();
        }
        let (tx, _rx) = ring_pair(Diagnostics::new());
        let host =
            NativeHost::new(Diagnostics::new(), tx).with_library_root(Some(base.join("lib")));
        let abs = |relative: &str| {
            relative
                .split('/')
                .fold(base.clone(), |path, part| path.join(part))
                .display()
                .to_string()
        };
        let open = abs("open/b.json");

        assert_eq!(
            host.offered(ListWhat::Instruments, Some(&open)),
            [
                abs("lib/a.json"),
                abs("lib/nested/n.json"),
                abs("open/b.json")
            ]
        );
        assert_eq!(
            host.offered(ListWhat::Samples, Some(&open)),
            [abs("lib/s.wav")]
        );
        assert_eq!(
            host.offered(ListWhat::Instruments, None),
            [abs("lib/a.json"), abs("lib/nested/n.json")]
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn within_is_the_library_root_and_the_open_dir_by_normalized_path() {
        let base = std::env::temp_dir().join("reuben-within-test");
        let (tx, _rx) = ring_pair(Diagnostics::new());
        let host =
            NativeHost::new(Diagnostics::new(), tx).with_library_root(Some(base.join("lib")));
        let abs = |relative: &str| {
            relative
                .split('/')
                .fold(base.clone(), |path, part| path.join(part))
                .display()
                .to_string()
        };
        let open = abs("open/b.json");
        let open = Some(open.as_str());

        assert!(host.within(&abs("lib/a.json"), open));
        assert!(host.within(&abs("lib/not/yet/there.json"), open));
        assert!(host.within(&abs("open/new.json"), open));
        assert!(host.within(&abs("lib/x/../a.json"), open));
        assert!(!host.within(&abs("lib/../elsewhere/c.json"), open));
        assert!(!host.within(&abs("open/../../escape.json"), open));
        assert!(!host.within("/etc/passwd.json", open));
        assert!(!host.within(&abs("lib2/a.json"), open));
        assert!(!host.within(&abs("open/new.json"), None));
    }

    /// A fresh, process-unique directory for `case`, and a joiner that spells a path under it the
    /// way the host does: absolute, `/`-separated parts joined one by one.
    fn scratch_dir(case: &str) -> (PathBuf, impl Fn(&str) -> String) {
        let base = std::env::temp_dir().join(format!("reuben-{case}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.clone();
        let abs = move |relative: &str| {
            relative
                .split('/')
                .fold(root.clone(), |path, part| path.join(part))
                .display()
                .to_string()
        };
        (base, abs)
    }

    #[test]
    fn library_lists_only_documents_directly_in_an_offered_dir() {
        let (base, abs) = scratch_dir("library-test");
        for dir in ["lib/voices", "open", "offered/deeper", "elsewhere"] {
            std::fs::create_dir_all(base.join(dir)).unwrap();
        }
        for file in [
            "lib/a.json",
            "lib/s.wav",
            "lib/voices/v.json",
            "open/b.json",
            "offered/c.json",
            "offered/deeper/d.json",
            "elsewhere/e.json",
        ] {
            std::fs::write(abs(file), "").unwrap();
        }
        let (tx, _rx) = ring_pair(Diagnostics::new());
        let host =
            NativeHost::new(Diagnostics::new(), tx).with_library_root(Some(base.join("lib")));
        host.offer_dir(&base.join("offered"));
        // Offered twice, once through the library root: still listed once.
        host.offer_dir(&base.join("lib/../lib"));
        let open = abs("open/b.json");

        assert_eq!(
            host.library(Some(&open)),
            [abs("lib/a.json"), abs("offered/c.json"), abs("open/b.json")]
        );
        assert_eq!(
            host.library(None),
            [abs("lib/a.json"), abs("offered/c.json")]
        );
        assert_eq!(
            host.creatable(Some(&open)),
            [abs("lib"), abs("offered"), abs("open")]
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn library_lists_a_symlinked_document() {
        let (base, abs) = scratch_dir("library-symlink-test");
        std::fs::create_dir_all(base.join("lib")).unwrap();
        std::fs::create_dir_all(base.join("elsewhere")).unwrap();
        std::fs::write(abs("elsewhere/real.json"), "").unwrap();
        std::os::unix::fs::symlink(abs("elsewhere/real.json"), abs("lib/linked.json")).unwrap();
        let (tx, _rx) = ring_pair(Diagnostics::new());
        let host =
            NativeHost::new(Diagnostics::new(), tx).with_library_root(Some(base.join("lib")));
        assert_eq!(host.library(None), [abs("lib/linked.json")]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_offered_dir_is_listed_and_within() {
        let (base, abs) = scratch_dir("offer-dir-test");
        std::fs::create_dir_all(base.join("offered/nested")).unwrap();
        std::fs::write(abs("offered/o.json"), "").unwrap();
        std::fs::write(abs("offered/nested/n.json"), "").unwrap();
        let (tx, _rx) = ring_pair(Diagnostics::new());
        let host = NativeHost::new(Diagnostics::new(), tx);
        assert!(!host.within(&abs("offered/new.json"), None));
        assert!(host.offered(ListWhat::Instruments, None).is_empty());

        host.offer_dir(&base.join("offered"));

        assert!(host.within(&abs("offered/new.json"), None));
        assert!(host.within(&abs("offered/nested/n.json"), None));
        assert!(!host.within(&abs("offered/../escape.json"), None));
        // An offered directory may be as wide as a home directory: only its own files list, also
        // while the open instrument is one of them.
        assert_eq!(
            host.offered(ListWhat::Instruments, None),
            [abs("offered/o.json")]
        );
        assert_eq!(
            host.offered(ListWhat::Instruments, Some(&abs("offered/o.json"))),
            [abs("offered/nested/n.json"), abs("offered/o.json")]
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn surface_written_bumps_the_route_watch_generation() {
        use reuben_api::surface::SurfaceKind;
        use std::sync::atomic::Ordering;
        let (tx, _rx) = ring_pair(Diagnostics::new());
        let host = NativeHost::new(Diagnostics::new(), tx);
        let generation = host.midi_map_generation();
        host.surface_written(SurfaceKind::Screen);
        assert_eq!(generation.load(Ordering::SeqCst), 0);
        host.surface_written(SurfaceKind::Midi);
        host.surface_written(SurfaceKind::Midi);
        assert_eq!(generation.load(Ordering::SeqCst), 2);
    }

    /// "Open file…", the command line and a recent each offer the file's own directory and then
    /// open the file: its voices and samples still list, and once another instrument is open the
    /// offered directory lists only its own files again.
    #[test]
    fn an_offered_dir_that_is_also_the_open_dir_lists_its_subdirectories() {
        let (base, abs) = scratch_dir("offer-open-dir-test");
        std::fs::create_dir_all(base.join("mine/voices")).unwrap();
        std::fs::create_dir_all(base.join("mine/samples")).unwrap();
        std::fs::create_dir_all(base.join("other")).unwrap();
        for file in [
            "mine/top.json",
            "mine/voices/v.json",
            "mine/samples/s.wav",
            "other/x.json",
        ] {
            std::fs::write(abs(file), "").unwrap();
        }
        let (tx, _rx) = ring_pair(Diagnostics::new());
        let host = NativeHost::new(Diagnostics::new(), tx);
        host.offer_dir(&base.join("mine"));
        let open = abs("mine/top.json");

        assert_eq!(
            host.offered(ListWhat::Instruments, Some(&open)),
            [abs("mine/top.json"), abs("mine/voices/v.json")]
        );
        assert_eq!(
            host.offered(ListWhat::Samples, Some(&open)),
            [abs("mine/samples/s.wav")]
        );
        let elsewhere = abs("other/x.json");
        assert_eq!(
            host.offered(ListWhat::Instruments, Some(&elsewhere)),
            [abs("mine/top.json"), abs("other/x.json")]
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn deliver_midi_routes_through_the_router() {
        use crate::control::ControlDrain;
        use crate::midi::router::Router;
        use crate::midi::routes::{Route, RouteTable, Scale};
        use reuben_api::surface::MidiKind;
        use reuben_api::surface::OscPattern;

        let diagnostics = Diagnostics::new();
        let (tx, _rx) = ring_pair(Arc::clone(&diagnostics));
        let (midi_tx, midi_rx) = ring_pair(Arc::clone(&diagnostics));
        let mut router = Router::new(midi_tx, Arc::clone(&diagnostics), false);
        router.install(RouteTable {
            routes: vec![Route {
                pattern: OscPattern::parse("/midi/1/cc/74").unwrap(),
                kind: MidiKind::Cc,
                target: "/cutoff/in".to_string(),
                scale: Scale::Linear {
                    min: 0.0,
                    max: 100.0,
                    int: false,
                },
            }],
        });
        let host = NativeHost::new(Arc::clone(&diagnostics), tx)
            .with_midi_router(Arc::new(Mutex::new(router)));

        assert_eq!(
            host.deliver_midi("/midi/1/cc/74", vec![Arg::F32(0.5)]),
            Ok(())
        );
        let mut sent = Vec::new();
        ControlDrain::new(vec![midi_rx]).drain(|batch| {
            sent.extend(batch.iter().map(|m| (m.address.clone(), m.args.clone())));
            true
        });
        assert_eq!(sent, vec![("/cutoff/in".to_string(), vec![Arg::F32(50.0)])]);

        let refused = host.deliver_midi("/midi/17/cc/1", vec![Arg::F32(0.5)]);
        assert!(
            refused
                .as_ref()
                .is_err_and(|e| e.contains("`/midi/17/cc/1 0.5`")),
            "{refused:?}"
        );
        assert_eq!(diagnostics.snapshot().midi_received, 1);
    }

    #[test]
    fn a_host_without_a_router_refuses_midi() {
        let (tx, _rx) = ring_pair(Diagnostics::new());
        let host = NativeHost::new(Diagnostics::new(), tx);
        assert!(host
            .deliver_midi("/midi/1/cc/74", vec![Arg::F32(0.5)])
            .is_err());
    }

    #[test]
    fn midi_surface_is_found_beside_the_on_screen_one() {
        let (base, abs) = scratch_dir("midi-surface-test");
        std::fs::create_dir_all(base.join("surfaces")).unwrap();
        std::fs::write(abs("surfaces/a.json"), "on screen").unwrap();
        std::fs::write(abs("surfaces/a.midi.json"), "midi").unwrap();
        let instrument = abs("a.json");
        assert_eq!(
            find_surface(Path::new(&instrument), SurfaceKind::Midi),
            Some(PathBuf::from(abs("surfaces/a.midi.json")))
        );
        assert_eq!(
            find_surface(Path::new(&instrument), SurfaceKind::Screen),
            Some(PathBuf::from(abs("surfaces/a.json")))
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_bare_relative_open_source_still_offers_its_directory() {
        let dir = std::env::current_dir().unwrap();
        let (tx, _rx) = ring_pair(Diagnostics::new());
        let host = NativeHost::new(Diagnostics::new(), tx);
        assert!(host.within(
            &dir.join("sibling.json").display().to_string(),
            Some("pad.json")
        ));
        assert!(!host.within(
            &dir.join("..").join("up.json").display().to_string(),
            Some("pad.json")
        ));
        assert!(host.offered_dirs(Some("pad.json")).contains(&dir));
    }

    #[test]
    fn tcp_subscriber_receives_events() {
        use reuben_api::engine::{
            ChangedBy, Event, TcpTransport, Transport, DEFAULT_CONNECT_TIMEOUT,
        };
        use reuben_api::resources::Resources;
        use reuben_api::staged::Staged;
        use reuben_api::FsResolver;

        // Process-unique: a fixed name collides between concurrent checkouts, and this test
        // deletes the directory it is about to use.
        let dir =
            std::env::temp_dir().join(format!("reuben_subscribe_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("case dir");
        let top = dir.join("top.json");
        std::fs::write(&top, BASE_DOC).expect("seed the open instrument");
        let source = top.display().to_string();

        let staged: Staged<Box<dyn Resources + Send + Sync>> =
            Staged::new(Box::new(FsResolver::for_instrument(&top)));
        let (coordinator, side, _warnings) =
            reuben_api::engine::install_initial(BASE_DOC, staged.clone(), cfg())
                .expect("initial install");
        let (control_tx, control_rx) = ring_pair(Diagnostics::new());
        let host = NativeHost::new(Diagnostics::new(), control_tx).with_render_config(Arc::new(
            HeadlessRenderConfig {
                opened_input_channels: 0,
            },
        ));
        let state = ChannelState::new(coordinator, Arc::new(host), served(staged))
            .with_installed_source(Some(source.clone()));
        let cb = FakeCallback::spawn(side, control_rx);
        let server = ChannelServer::bind("127.0.0.1:0", state).expect("bind");
        let transport = TcpTransport::new(server.local_addr().to_string(), DEFAULT_CONNECT_TIMEOUT);

        // Read on a thread of its own: a stream with no event blocks for good, and a regression
        // should fail the test, not hang it.
        let mut events = transport.subscribe().expect("subscribed");
        let (first_tx, first_rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let _ = first_tx.send(events.next());
            events.next()
        });
        let edit = Request::Call {
            tool: "set_instrument_name".to_string(),
            args: serde_json::json!({"source": source, "name": "renamed"}),
        };
        let reply = transport
            .round_trip(&edit.to_ndjson(), Duration::from_secs(10))
            .expect("answered");
        let output = match Response::from_ndjson(&reply).expect("a response") {
            Response::Called { output, .. } => output,
            other => panic!("expected a called answer, got {other:?}"),
        };
        assert_eq!(output["written"], true, "{output}");

        match first_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("an event within the deadline")
        {
            Some(Ok(Event::Changed { hash, by, .. })) => {
                assert_eq!(by, ChangedBy::Client);
                assert_eq!(serde_json::json!(hash), output["hash"]);
            }
            other => panic!("expected a changed event, got {other:?}"),
        }

        // Shutdown joins the subscriber's handler too, which closes the stream.
        server.shutdown();
        assert!(reader.join().expect("reader").is_none());
        cb.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Poll `done` until it holds, failing the test past a deadline rather than hanging it.
    fn within_deadline(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "{what} within the deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_subscriber_that_disconnects_while_idle_is_dropped() {
        use reuben_api::engine::{TcpTransport, DEFAULT_CONNECT_TIMEOUT};

        let (state, cb, _) = swap_fixture(BASE_DOC, 0);
        let server = ChannelServer::bind("127.0.0.1:0", state.clone()).expect("bind");
        let transport = TcpTransport::new(server.local_addr().to_string(), DEFAULT_CONNECT_TIMEOUT);

        let events = transport.subscribe().expect("subscribed");
        assert_eq!(state.subscribers(), 1);
        drop(events);
        // Nothing is published: the handler has to notice the close itself.
        within_deadline("the subscription is dropped", || state.subscribers() == 0);

        server.shutdown();
        cb.stop();
    }

    #[test]
    fn a_request_after_subscribe_ends_the_stream() {
        use std::io::{BufRead, Read};

        let (state, cb, _) = swap_fixture(BASE_DOC, 0);
        let server = ChannelServer::bind("127.0.0.1:0", state.clone()).expect("bind");
        let stream = TcpStream::connect(server.local_addr()).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        let mut reader = BufReader::new(&stream);
        (&stream)
            .write_all(Request::Subscribe.to_ndjson().as_bytes())
            .expect("subscribe");
        let mut line = String::new();
        reader.read_line(&mut line).expect("reply");
        assert_eq!(
            Response::from_ndjson(&line).expect("a response"),
            Response::Subscribed
        );

        (&stream)
            .write_all(Request::Ping.to_ndjson().as_bytes())
            .expect("ping");
        // The server closes with the ping unread, which may reset the connection rather than end
        // it cleanly; either way nothing answers it.
        let mut rest = Vec::new();
        match reader.read_to_end(&mut rest) {
            Ok(_) => assert!(rest.is_empty(), "no answer to a request on a stream"),
            Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::ConnectionReset, "{e}"),
        }
        within_deadline("the subscription is dropped", || state.subscribers() == 0);

        server.shutdown();
        cb.stop();
    }

    /// A peer elsewhere that sends no first line, or only blank ones, is answered and closed once
    /// its admission deadline passes. Skipped, saying so, on a machine with no route off loopback.
    #[test]
    fn a_peer_elsewhere_that_never_presents_the_token_is_closed() {
        use std::io::BufRead;
        use std::net::{SocketAddr, UdpSocket};

        // This machine's address on the interface its default route leaves by. Connecting a UDP
        // socket sends nothing; it only picks the route.
        let ip = UdpSocket::bind("0.0.0.0:0")
            .and_then(|s| s.connect("192.0.2.1:9").and_then(|()| s.local_addr()))
            .map(|a| a.ip())
            .ok()
            .filter(|ip| !ip.is_loopback() && !ip.is_unspecified());
        let Some(ip) = ip else {
            eprintln!("skipped: no non-loopback interface");
            return;
        };

        let (state, cb, _) = swap_fixture(BASE_DOC, 0);
        let token = Token::new("s3cret").expect("a token");
        let server = ChannelServer::bind_admitting(
            "0.0.0.0:0",
            state,
            Some(token),
            Duration::from_millis(50),
        )
        .expect("bind");
        let addr = SocketAddr::new(ip, server.local_addr().port());

        // Silent, and blank lines: neither is a first line, so neither may hold the connection.
        for blank in ["", "\n\n  \n"] {
            let stream = TcpStream::connect(addr).expect("connect");
            (&stream).write_all(blank.as_bytes()).expect("write");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("a read timeout");
            let mut reply = String::new();
            BufReader::new(&stream)
                .read_line(&mut reply)
                .expect("the engine answers before closing");
            assert!(reply.contains(TOKEN_REQUIRED), "{blank:?}: {reply:?}");
            let mut rest = String::new();
            assert_eq!(
                BufReader::new(&stream)
                    .read_line(&mut rest)
                    .expect("closed"),
                0,
                "{blank:?}: the connection is closed"
            );
        }

        server.shutdown();
        cb.stop();
    }

    #[test]
    fn ping_is_answered_with_pong() {
        let (state, cb, _) = swap_fixture(BASE_DOC, 0);
        assert_eq!(dispatch(&state, &Request::Ping.to_ndjson()), Response::Pong);
        cb.stop();
    }

    #[test]
    fn get_document_returns_the_coordinators_hash() {
        let (state, cb, base_hash) = swap_fixture(BASE_DOC, 0);
        match dispatch(&state, &Request::GetDocument.to_ndjson()) {
            Response::Document(snapshot) => {
                assert_eq!(snapshot.content_hash, base_hash);
            }
            other => panic!("expected Document, got {other:?}"),
        }
        cb.stop();
    }

    /// The `source` half of the snapshot: seeded by `play`'s path, re-pointed by an open that
    /// installs, and — the part worth a test — **left alone by one that reads nothing**, so
    /// `get_document` never names a document that was not opened.
    #[test]
    fn installed_source_follows_the_open_and_an_unreadable_one_leaves_it_alone() {
        fn source_of(state: &ChannelState) -> Option<String> {
            match dispatch(state, &Request::GetDocument.to_ndjson()) {
                Response::Document(snapshot) => snapshot.source,
                other => panic!("expected Document, got {other:?}"),
            }
        }

        let (state, cb, _, library) = open_fixture(BASE_DOC, "installed_source");
        let base = library.0.join("base.json").display().to_string();
        assert_eq!(source_of(&state), Some(base));

        let good = library.0.join("next.json").display().to_string();
        match dispatch(
            &state,
            &library
                .open("next.json", &envelope_doc("/eg"), None)
                .to_ndjson(),
        ) {
            Response::SwapReport(report) => assert!(report.report.ok, "{report:?}"),
            other => panic!("expected SwapReport, got {other:?}"),
        }
        assert_eq!(source_of(&state), Some(good.clone()));

        let missing = Request::Open {
            name: library.0.join("nope.json").display().to_string(),
            expect: None,
        };
        match dispatch(&state, &missing.to_ndjson()) {
            Response::SwapReport(report) => assert!(!report.report.ok, "{report:?}"),
            other => panic!("expected SwapReport, got {other:?}"),
        }
        assert_eq!(
            source_of(&state),
            Some(good),
            "an unreadable open must leave `source` naming what is open"
        );
        cb.stop();
    }

    #[test]
    fn a_probed_node_is_metered_through_the_real_channel() {
        use reuben_api::engine::{Channel, InProcess};
        let (state, cb, _) = swap_fixture(BASE_DOC, 0);
        let channel = Channel::new(InProcess::new(state));
        assert!(channel.get_meters().expect("meters").probes.is_empty());
        let resolved = channel
            .set_probes(vec![vec!["/osc".to_string()], vec!["/nope".to_string()]])
            .expect("set_probes");
        assert_eq!(resolved, vec![vec!["/osc".to_string()]]);

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let probes = channel.get_meters().expect("meters").probes;
            assert_eq!(probes.len(), 1);
            if probes[0].ports[0].level.peak_dbfs > -120.0 {
                assert_eq!(probes[0].path, vec!["/osc".to_string()]);
                break;
            }
            assert!(Instant::now() < deadline, "no level within 5 s: {probes:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
        cb.stop();
    }

    #[test]
    fn a_host_switches_off_probes_nobody_renews() {
        let staged = no_store();
        let (coordinator, side, _w) =
            reuben_api::engine::install_initial(BASE_DOC, staged.clone(), cfg()).expect("install");
        let slots = coordinator.installed_probes();
        let (control_tx, control_rx) = ring_pair(Diagnostics::new());
        let host = Arc::new(NativeHost::with_probe_lease(
            Diagnostics::new(),
            control_tx,
            Duration::from_millis(30),
            Duration::from_millis(5),
        ));
        let state = ChannelState::new(coordinator, host.clone(), served(staged));
        let cb = FakeCallback::spawn(side, control_rx);
        let resolved = EngineHost::set_probes(&*host, vec![vec!["/osc".to_string()]]).unwrap();
        assert_eq!(resolved.len(), 1);
        assert!(slots.any_on());
        // Nothing calls `meters()`, so only the host's own lease thread can expire the set.
        let deadline = Instant::now() + Duration::from_secs(5);
        while slots.any_on() {
            assert!(Instant::now() < deadline, "the host never expired the set");
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(state);
        cb.stop();
    }

    #[test]
    fn get_diagnostics_reads_the_live_counters() {
        let staged = no_store();
        let (coordinator, side, _w) =
            reuben_api::engine::install_initial(BASE_DOC, staged.clone(), cfg()).expect("install");
        let diagnostics = Diagnostics::new();
        // This verb never touches control, but the ingress is a constructor parameter now — there
        // is no such thing as a host that cannot serve `send`.
        let (control_tx, control_rx) = ring_pair(Diagnostics::new());
        let state = ChannelState::new(
            coordinator,
            Arc::new(NativeHost::new(Arc::clone(&diagnostics), control_tx)),
            served(staged),
        );
        let cb = FakeCallback::spawn(side, control_rx);
        // Fresh: zeroed.
        assert_eq!(
            dispatch(&state, &Request::GetDiagnostics.to_ndjson()),
            Response::Diagnostics(DiagnosticsReport::default())
        );
        // A later bump is visible — the live Arc is read, not a frozen copy.
        diagnostics.record_output_xrun();
        assert_eq!(
            dispatch(&state, &Request::GetDiagnostics.to_ndjson()),
            Response::Diagnostics(DiagnosticsReport {
                output_xruns: 1,
                ..DiagnosticsReport::default()
            })
        );
        cb.stop();
    }

    #[test]
    fn open_installs_via_the_mailbox_with_real_survivor_stats() {
        // An open of an identical envelope document keeps both nodes survivors, so the real
        // migration diff carries `survived: 2` — impossible under an all-cold restart (which would
        // hard-code `survived: 0`). The open installed via the mailbox (the FakeCallback drained
        // it; `reclaim` completing is the proof), no stream teardown involved.
        let base = envelope_doc("/env");
        let (state, cb, base_hash, library) = open_fixture(&base, "survivors");

        match dispatch(&state, &library.open("same.json", &base, None).to_ndjson()) {
            Response::SwapReport(report) => {
                assert!(report.report.ok, "a valid document installs: {report:?}");
                let diff = report.diff.expect("a successful swap carries a diff");
                assert_eq!(
                    diff.survived, 2,
                    "both nodes survive an identical-document swap (real migration): {diff:?}"
                );
                // Same document -> same hash; the report names what is now playing.
                assert_eq!(report.content_hash, base_hash);
            }
            other => panic!("expected SwapReport, got {other:?}"),
        }
        cb.stop();
    }

    #[test]
    fn open_of_a_renamed_node_resets_it_and_reports_a_smaller_survivor_count() {
        // The reset half: renaming the envelope makes it a remove+add, so only `/out` survives —
        // `survived: 1`, with `/env` removed and `/eg` added. Real survivor semantics from the
        // manifest diff, not a blanket zero.
        let (state, cb, _base_hash, library) = open_fixture(&envelope_doc("/env"), "renamed");
        match dispatch(
            &state,
            &library
                .open("eg.json", &envelope_doc("/eg"), None)
                .to_ndjson(),
        ) {
            Response::SwapReport(report) => {
                assert!(report.report.ok, "{report:?}");
                let diff = report.diff.expect("diff");
                assert_eq!(diff.survived, 1, "only /out survives a rename: {diff:?}");
                assert!(diff.removed.contains(&"/env".to_string()), "{diff:?}");
                assert!(diff.added.contains(&"/eg".to_string()), "{diff:?}");
            }
            other => panic!("expected SwapReport, got {other:?}"),
        }
        cb.stop();
    }

    #[test]
    fn back_to_back_opens_all_install_proving_off_thread_reclaim() {
        // One swap in flight: a second open can only install once the first's retiree
        // has come home and been reclaimed. All succeeding is the behavioral proof the mailbox +
        // off-thread reclaim cycle actually turned over, not a restart.
        let (state, cb, _, library) = open_fixture(&envelope_doc("/env"), "back_to_back");
        for (file, target) in [
            ("a.json", envelope_doc("/env")),
            ("b.json", envelope_doc("/eg")),
            ("c.json", envelope_doc("/env")),
        ] {
            match dispatch(&state, &library.open(file, &target, None).to_ndjson()) {
                Response::SwapReport(report) => assert!(report.report.ok, "{report:?}"),
                other => panic!("expected SwapReport, got {other:?}"),
            }
        }
        cb.stop();
    }

    #[test]
    fn input_binding_open_onto_output_only_stream_dark_degrades_with_a_warning() {
        // The initial rig is output-only (`opened_input_channels: 0`). An open of an
        // instrument that binds an input channel installs and stays silent-but-alive — a loud
        // swap-report WARNING, never an error or a crash.
        let (state, cb, _, library) = open_fixture(BASE_DOC, "dark_degrade");
        match dispatch(
            &state,
            &library.open("mic.json", MIC_PASSTHRU, None).to_ndjson(),
        ) {
            Response::SwapReport(report) => {
                assert!(
                    report.report.ok,
                    "dark-degrade is not an error — the engine stays alive: {report:?}"
                );
                assert!(
                    report
                        .report
                        .warnings
                        .iter()
                        .any(|w| w.message.contains("dark-degrade")),
                    "the swap report carries the loud dark-degrade warning: {report:?}"
                );
            }
            other => panic!("expected SwapReport, got {other:?}"),
        }
        cb.stop();
    }

    #[test]
    fn open_with_a_stale_expect_conflicts_and_does_not_install() {
        let (state, cb, base_hash, library) = open_fixture(BASE_DOC, "stale_expect");
        let req = library.open(
            "env.json",
            &envelope_doc("/env"),
            Some("0badc0de0badc0de".to_string()),
        );
        match dispatch(&state, &req.to_ndjson()) {
            Response::Conflict(conflict) => {
                assert_eq!(conflict.expected, "0badc0de0badc0de");
                assert_eq!(
                    conflict.actual, base_hash,
                    "conflict names the real installed hash"
                );
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
        // The installed document is unchanged.
        match dispatch(&state, &Request::GetDocument.to_ndjson()) {
            Response::Document(snapshot) => assert_eq!(snapshot.content_hash, base_hash),
            other => panic!("expected Document, got {other:?}"),
        }
        cb.stop();
    }

    #[test]
    fn open_with_a_matching_expect_succeeds() {
        let (state, cb, base_hash, library) = open_fixture(BASE_DOC, "matching_expect");
        match dispatch(
            &state,
            &library
                .open("env.json", &envelope_doc("/env"), Some(base_hash))
                .to_ndjson(),
        ) {
            Response::SwapReport(report) => assert!(report.report.ok, "{report:?}"),
            other => panic!("expected SwapReport, got {other:?}"),
        }
        cb.stop();
    }

    /// A batch holding a message compiled for a Plan that a swap has since replaced reaches the
    /// callback after the install: the whole batch is dropped rather than queued, every message
    /// counted, and every address named to subscribers by the engine's watch thread, through the
    /// real ring, drain and producer.
    #[test]
    fn a_send_compiled_for_a_replaced_plan_is_dropped_counted_and_named() {
        use crate::engine::DroppedSendWatch;
        use reuben_api::engine::{Event, PreparedAgainst};

        let diagnostics = Diagnostics::new();
        let (state, side, control_rx, _, library, host) =
            open_parts_counting(BASE_DOC, "stale_send", Arc::clone(&diagnostics));
        let replaced = PreparedAgainst::new(Arc::clone(side.engine.prepared_routes()));
        let cb = FakeCallback::counting(side, control_rx, Arc::clone(&diagnostics));
        let _watch = DroppedSendWatch::spawn(state.clone(), Arc::clone(&diagnostics))
            .expect("spawn the watch thread");
        let events = state.subscribe();

        match dispatch(
            &state,
            &library.open("next.json", BASE_DOC, None).to_ndjson(),
        ) {
            Response::SwapReport(report) => assert!(report.report.ok, "{report:?}"),
            other => panic!("expected SwapReport, got {other:?}"),
        }
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(2)),
            Ok(Event::Opened { .. })
        ));

        // The host's own ingress, as `send` reaches it, carrying what a send compiled before the
        // open would carry.
        let stale = vec![
            PreparedMessage {
                address: "/osc/freq".to_string(),
                args: vec![Arg::F32(220.0)],
                against: Some(replaced),
            },
            PreparedMessage {
                address: "/osc/amp".to_string(),
                args: vec![Arg::F32(0.5)],
                against: None,
            },
        ];
        assert_eq!(host.deliver_control(stale), Ok(()));

        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)),
            Ok(Event::SendsDropped {
                addresses: vec!["/osc/amp".to_string(), "/osc/freq".to_string()],
            }),
            "the whole batch is dropped and every address named"
        );
        assert_eq!(diagnostics.snapshot().stale_sends_dropped, 2);
        cb.stop();
    }

    fn one_send() -> Vec<PreparedMessage> {
        vec![PreparedMessage {
            address: "/osc/freq".to_string(),
            args: vec![Arg::F32(440.0)],
            against: None,
        }]
    }

    /// A `send` the full ring refuses is dropped and counted on the host's diagnostics, and the
    /// verb still answers: nothing waits on the render callback.
    #[test]
    fn a_send_against_a_full_ring_is_dropped_and_counted() {
        let diagnostics = Diagnostics::new();
        let (control, _ring) = ring_pair(Arc::clone(&diagnostics));
        let host = NativeHost::new(Arc::clone(&diagnostics), control);
        for _ in 0..crate::control::CONTROL_RING_BATCHES + 2 {
            assert_eq!(host.deliver_control(one_send()), Ok(()));
        }
        let report = host.diagnostics();
        assert_eq!(report.control_batches_dropped_full, 2);
        assert_eq!(report.output_xruns, 0);
    }

    #[test]
    fn a_send_after_the_render_side_is_gone_finds_the_ingress_closed() {
        let (control, ring) = ring_pair(Diagnostics::new());
        let host = NativeHost::new(Diagnostics::new(), control);
        drop(ring);
        assert_eq!(host.deliver_control(one_send()), Err(IngressClosed));
    }

    #[test]
    fn send_delivers_the_whole_batch_to_the_engines_queue_osc() {
        // Control converges where external OSC does. The assertion is on what reached
        // `queue_osc` — the exact call a decoded UDP datagram reaches — so this proves the door
        // delivered `{address, [Arg]}` to the engine, not merely that a sink was poked. Distinct
        // values per atom kind so a mis-mapped variant can't pass.
        let (state, cb, _) = swap_fixture(BASE_DOC, 0);
        let request = Request::Send {
            messages: vec![
                ControlMessage {
                    address: "/osc/freq".to_string(),
                    args: vec![ControlArg::F32(440.5)],
                },
                ControlMessage {
                    address: "/osc/shape".to_string(),
                    args: vec![ControlArg::Str("Up".to_string()), ControlArg::I32(3)],
                },
            ],
        };
        assert_eq!(
            dispatch(&state, &request.to_ndjson()),
            Response::Sent,
            "a live engine acks the whole batch"
        );

        let queued = cb.await_queued(2);
        assert_eq!(
            queued,
            vec![
                OscIn::new("/osc/freq".to_string(), vec![Arg::F32(440.5)]),
                OscIn::new(
                    "/osc/shape".to_string(),
                    vec![Arg::Str("Up".into()), Arg::I32(3)]
                ),
            ],
            "both messages arrive in order, each atom as its primitive Arg"
        );
        cb.stop();
    }

    #[test]
    fn send_acks_an_unroutable_address_because_the_engine_drops_it_silently() {
        // The ack is "queued", never "applied": an address matching no node/port is dropped at the
        // engine's ingress exactly as a stale external datagram is, and the channel does not
        // pre-judge it — this door has no routing table and must not grow one.
        let (state, cb, _) = swap_fixture(BASE_DOC, 0);
        let request = Request::Send {
            messages: vec![ControlMessage {
                address: "/nowhere/at/all".to_string(),
                args: vec![ControlArg::F32(1.0)],
            }],
        };
        assert_eq!(dispatch(&state, &request.to_ndjson()), Response::Sent);
        assert_eq!(cb.await_queued(1).len(), 1, "it still reached queue_osc");
        cb.stop();
    }

    #[test]
    fn send_hands_the_batch_to_the_callback_as_one_unit() {
        // The atomicity the wire promises is a property of HOW the batch is handed over: one push,
        // so concurrent handler threads cannot interleave into each other's gestures and the
        // callback cannot split one across two blocks. Asserting on the drained batch SIZES is what
        // distinguishes that from three single-message pushes that happen to arrive in order.
        let (state, cb, _) = swap_fixture(BASE_DOC, 0);
        let request = Request::Send {
            messages: (0..3)
                .map(|i| ControlMessage {
                    address: format!("/osc/p{i}"),
                    args: vec![ControlArg::I32(i)],
                })
                .collect(),
        };
        assert_eq!(dispatch(&state, &request.to_ndjson()), Response::Sent);
        cb.await_queued(3);
        assert_eq!(
            cb.drained_batch_sizes(),
            vec![3],
            "the gesture crossed as ONE batch, not three separate pushes"
        );
        cb.stop();
    }

    #[test]
    fn send_rejects_an_empty_batch_rather_than_acking_a_no_op() {
        // A client bug that drops its messages must not read as a working send. The tool surface
        // rejects this too, but the channel is its own contract — netcat and any future door reach
        // it without going through that tool.
        let (state, cb, _) = swap_fixture(BASE_DOC, 0);
        let request = Request::Send {
            messages: Vec::new(),
        };
        match dispatch(&state, &request.to_ndjson()) {
            Response::Error { message } => assert!(
                message.contains("at least one"),
                "the error names what is wrong: {message}"
            ),
            other => panic!("expected Error, got {other:?}"),
        }
        cb.stop();
    }

    #[test]
    fn send_rejects_a_batch_over_the_limit_before_it_reaches_the_callback() {
        // The batch lands whole in one render callback, so its size is an RT concern, not just a
        // request-size one. The limit is enforced here — the door advertising it is a courtesy, not
        // the guarantee — and nothing is queued when it trips.
        let (state, cb, _) = swap_fixture(BASE_DOC, 0);
        let request = Request::Send {
            messages: (0..MAX_SEND_BATCH + 1)
                .map(|i| ControlMessage {
                    address: format!("/osc/p{i}"),
                    args: vec![ControlArg::I32(0)],
                })
                .collect(),
        };
        match dispatch(&state, &request.to_ndjson()) {
            Response::Error { message } => assert!(
                message.contains(&MAX_SEND_BATCH.to_string()),
                "the error names the limit so a client can split: {message}"
            ),
            other => panic!("expected Error, got {other:?}"),
        }
        assert!(
            cb.drained_batch_sizes().is_empty(),
            "an over-limit batch reaches the callback not at all"
        );
        // The limit itself is servable — the boundary is inclusive.
        let ok = Request::Send {
            messages: (0..MAX_SEND_BATCH)
                .map(|i| ControlMessage {
                    address: format!("/osc/p{i}"),
                    args: vec![ControlArg::I32(0)],
                })
                .collect(),
        };
        assert_eq!(dispatch(&state, &ok.to_ndjson()), Response::Sent);
        cb.stop();
    }

    #[test]
    fn an_unreadable_line_is_a_channel_error() {
        let (state, cb, _) = swap_fixture(BASE_DOC, 0);
        match dispatch(&state, "{not json}\n") {
            Response::Error { message } => assert!(message.contains("unreadable request")),
            other => panic!("a malformed request must return Error, got {other:?}"),
        }
        cb.stop();
    }

    #[test]
    fn an_input_stream_fits_only_an_engine_binding_exactly_its_width() {
        assert!(input_stream_fits(0, 0), "no input, no stream");
        assert!(input_stream_fits(2, 2));
        assert!(!input_stream_fits(1, 0), "input bound, no stream");
        assert!(!input_stream_fits(0, 1), "a stream nothing reads");
        assert!(
            !input_stream_fits(1, 2),
            "the callback feeds only an exact width"
        );
    }

    #[test]
    fn dark_degrade_warning_fires_only_on_unmatched_input_geometry() {
        // The pure rule: a warning on any width mismatch, none when the widths match.
        assert!(
            dark_degrade_warning(0, 0).is_empty(),
            "no input, no warning"
        );
        assert!(
            dark_degrade_warning(2, 2).is_empty(),
            "matched geometry is silent"
        );
        assert!(
            !dark_degrade_warning(1, 0).is_empty(),
            "input onto output-only stream warns"
        );
        assert!(
            !dark_degrade_warning(3, 2).is_empty(),
            "a wider input than the stream warns"
        );
        assert!(
            !dark_degrade_warning(1, 2).is_empty(),
            "the callback feeds only an exact width, so a wider stream feeds nothing"
        );
        assert!(
            !dark_degrade_warning(0, 1).is_empty(),
            "a stream nothing reads is reported"
        );
    }

    #[test]
    fn liveness_grace_scales_with_the_callback_period_and_clamps_to_the_bounds() {
        let hard = Duration::from_millis(500);
        // Floor: a tiny buffer's 4× period is below the floor, so the floor wins.
        assert_eq!(
            liveness_grace(Some(Duration::from_millis(3)), hard),
            SWAP_LIVENESS_GRACE_FLOOR,
            "4×3ms is under the 50ms floor"
        );
        // Mid: a fat buffer scales linearly (4 × 93ms ≈ 372ms, between floor and hard).
        assert_eq!(
            liveness_grace(Some(Duration::from_millis(93)), hard),
            Duration::from_millis(372),
            "4×93ms sits between the bounds"
        );
        // Ceiling: a large-buffer / low-rate device (256ms period) would want
        // 4×256ms = 1.024s, clamped to the hard deadline — never cut off earlier than the old bound.
        assert_eq!(
            liveness_grace(Some(Duration::from_millis(256)), hard),
            hard,
            "the grace never exceeds the hard deadline"
        );
        // No reported period (headless): the floor.
        assert_eq!(liveness_grace(None, hard), SWAP_LIVENESS_GRACE_FLOOR);
    }

    #[test]
    fn swap_poll_gate_bails_when_the_heartbeat_goes_flat_and_re_arms_on_every_tick() {
        // Deterministic clock (fixed base + offsets) so the gate's decision is tested without
        // sleeping. A ~10ms period ⇒ 40ms grace, clamped up to the 50ms floor.
        let t0 = Instant::now();
        let hard = Duration::from_millis(500);
        let live = |callbacks| {
            Some(RenderLiveness {
                callbacks,
                callback_period: Duration::from_millis(10),
            })
        };
        let grace = SWAP_LIVENESS_GRACE_FLOOR; // 50ms here

        // Stopped at the start: the heartbeat never advances, so bail once flat past the grace — not
        // the hard deadline.
        let mut dead = SwapPollGate::start_at(live(7), t0, hard);
        assert!(
            !dead.give_up_at(Some(7), t0 + Duration::from_millis(10)),
            "before the grace: keep polling"
        );
        assert!(
            dead.give_up_at(Some(7), t0 + grace + Duration::from_millis(1)),
            "flat past the grace: give up"
        );

        // Live: each observed tick re-arms the flat-window, so the poll runs to the hard deadline no
        // matter how long the individual blocks are.
        let mut lively = SwapPollGate::start_at(live(7), t0, hard);
        assert!(
            !lively.give_up_at(Some(8), t0 + Duration::from_millis(40)),
            "a tick before the grace re-arms the window"
        );
        assert!(
            !lively.give_up_at(Some(9), t0 + Duration::from_millis(80)),
            "another tick keeps re-arming past the original grace"
        );
        assert!(
            lively.give_up_at(Some(9), t0 + hard),
            "still bounded by the hard deadline"
        );

        // Dies mid-poll: ticks once, then goes flat — bail a grace after the LAST tick,
        // not the full deadline. Last tick observed at 40ms ⇒ bail by ~90ms.
        let mut stalled = SwapPollGate::start_at(live(7), t0, hard);
        assert!(!stalled.give_up_at(Some(8), t0 + Duration::from_millis(40)));
        assert!(
            !stalled.give_up_at(Some(8), t0 + Duration::from_millis(80)),
            "40ms flat since the last tick: still under the grace"
        );
        assert!(
            stalled.give_up_at(
                Some(8),
                t0 + Duration::from_millis(40) + grace + Duration::from_millis(1)
            ),
            "a grace flat since the last tick: give up well before the hard deadline"
        );

        // Headless (no heartbeat at all): the window never re-arms, so bail at the grace.
        let mut headless = SwapPollGate::start_at(None, t0, hard);
        assert!(!headless.give_up_at(None, t0 + Duration::from_millis(10)));
        assert!(headless.give_up_at(None, t0 + grace + Duration::from_millis(1)));
    }

    #[test]
    fn swap_reclaim_bails_fast_when_no_render_side_consumes() {
        // With nothing draining the mailbox (audio has stopped), the engine reclaim would otherwise
        // spin the full SWAP_RECLAIM_TIMEOUT under the Coordinator lock, stalling get_document and
        // the next swap. No FakeCallback here — the render side is dropped, so
        // the retiree never comes home — so the liveness gate must bail at the grace instead.
        let (state, _side, _control_rx, _, library) = open_parts(BASE_DOC, "reclaim_bails");
        let open = library.open("env.json", &envelope_doc("/env"), None);

        let start = Instant::now();
        let resp = dispatch(&state, &open.to_ndjson());
        let elapsed = start.elapsed();

        assert!(
            matches!(resp, Response::SwapReport(ref r) if r.report.ok),
            "the open still commits and returns its report: {resp:?}"
        );
        assert!(
            elapsed < SWAP_RECLAIM_TIMEOUT,
            "reclaim bailed at the liveness grace, not the full {SWAP_RECLAIM_TIMEOUT:?} \
             (took {elapsed:?})"
        );
    }
}

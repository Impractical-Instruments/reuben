//! The engine the editor plays through. Either embedded — everything `reuben play` starts, audio
//! out, OSC-in, the loopback engine channel `reuben-mcp` dials, owned by the editor for as long
//! as its window is open — or [attached](Engine::attach) to a `reuben play` already running, which
//! the editor reaches over that channel and owns nothing of.
//!
//! Starting never panics. Whatever [`reuben_native::engine::start`] reports becomes a [`Line`] the
//! window shows, printed to the terminal as well.
//!
//! The audio streams are fixed when the engine starts, so a change of device profile is a
//! [`restart`](Engine::restart_with): the engine stops, frees its ports, and starts again on the
//! instrument it was playing. Voice state and uncommitted live values do not survive it.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use reuben_api::authoring::{Report, ValidateInstrument};
use reuben_api::engine::{call_tool, Channel, Installed, SwapOutcome, Token};
use reuben_api::tools::names;
use reuben_native::engine::{self, EngineConfig, Instrument, RunningEngine, StartError};
use reuben_native::engine_channel::input_stream_fits;
use reuben_native::profile::DeviceProfile;

use crate::link::{Change, Link};
use crate::sends::Live;

/// How serious a [`Line`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warning,
    Error,
}

/// One thing starting the engine reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub level: Level,
    pub text: String,
}

impl Line {
    fn new(level: Level, text: impl Into<String>) -> Self {
        Self {
            level,
            text: text.into(),
        }
    }
}

impl fmt::Display for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.level {
            Level::Info => write!(f, "{}", self.text),
            Level::Warning => write!(f, "warning: {}", self.text),
            Level::Error => write!(f, "error: {}", self.text),
        }
    }
}

/// What an attached engine plays: the file it was installed from, and the content hash of what it
/// installed from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playing {
    pub source: String,
    pub content_hash: String,
}

/// The embedded engine, when one is running, and what starting it reported; or the link to an
/// attached one.
///
/// Not `Send`, because [`RunningEngine`] is not: it lives on the UI thread that started it.
pub struct Engine {
    running: Option<RunningEngine>,
    /// Set only by [`attach`](Self::attach), and then `running` and `config` stay `None` for good:
    /// what restarts or refits an embedded engine finds nothing to act on.
    attached: Option<Link>,
    /// What the attached engine was last seen to play, until the editor follows it.
    unfollowed: Option<Playing>,
    /// What the last start was asked for, kept whether or not it succeeded, so a restart can try
    /// again with another profile. `None` when no start was ever attempted.
    config: Option<EngineConfig>,
    report: Vec<Line>,
    /// The installed document (source, content hash) a restart to fit its input was last tried
    /// for, on this run of the engine. A restart that could not happen is not tried again every
    /// frame.
    refit_tried: Option<(Option<String>, String)>,
    /// The file the engine was last seen to have installed, and the hash of what it installed from
    /// it: kept through a Swap that holds it, and `None` while that is not known or the document
    /// has no file.
    plays: Option<Playing>,
    /// The directories offered to the server beyond its own, offered again to the server a
    /// restart starts.
    offered: Vec<PathBuf>,
    /// The engine was stopped and started again since [`take_restarted`](Self::take_restarted):
    /// the new server's history holds none of the old one's states.
    restarted: bool,
}

impl Engine {
    /// No engine: nothing runs and no port is bound.
    #[cfg(test)]
    pub(crate) fn idle() -> Self {
        Self {
            running: None,
            attached: None,
            unfollowed: None,
            config: None,
            report: Vec::new(),
            refit_tried: None,
            plays: None,
            offered: Vec::new(),
            restarted: false,
        }
    }

    /// Attach to the `reuben play` whose engine channel is at `addr`, starting nothing, opening
    /// every connection with `token` when given. `wake` is called from another thread whenever
    /// [`follow_link`](Self::follow_link) has news, and when a live send to it is answered.
    pub fn attach(
        addr: impl Into<String>,
        token: Option<Token>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let link = Link::dial(addr, token, wake);
        let line = Line::new(
            Level::Info,
            format!("attached to the engine on {}", link.addr()),
        );
        Self {
            attached: Some(link),
            ..Self::reported(None, None, vec![line])
        }
    }

    /// The link to the engine, when this one is attached rather than embedded.
    pub fn link(&self) -> Option<&Link> {
        self.attached.as_ref()
    }

    /// Take in what the attached engine's link found since the last call: what it plays, when that
    /// changed — on the first probe too, and after it stopped answering — and is a file the editor
    /// can open, is kept for [`unfollowed`](Self::unfollowed); the report names it, or says why it
    /// cannot be opened. Never waits; does nothing when not attached.
    pub fn follow_link(&mut self) {
        let Some(link) = self.attached.as_ref() else {
            return;
        };
        let addr = link.addr().to_string();
        for change in link.changes() {
            self.unfollowed = None;
            self.plays = match &change {
                Change::Up {
                    source,
                    content_hash,
                } => source.clone().map(|source| Playing {
                    source,
                    content_hash: content_hash.clone(),
                }),
                Change::Down(_) => None,
            };
            let unopenable = match change {
                Change::Up {
                    source: Some(source),
                    ..
                } if Path::new(&source).is_relative() => Some(format!(
                    "plays {source}, which is relative to the directory it was started in, so \
                     there is no knowing which file it is from here: name it by an absolute path"
                )),
                Change::Up {
                    source: Some(source),
                    content_hash,
                } => {
                    self.name_instrument(&source);
                    self.unfollowed = Some(Playing {
                        source,
                        content_hash,
                    });
                    None
                }
                Change::Up { source: None, .. } => Some(
                    "plays the built-in default rig, started without a file, so there is no file \
                     to open"
                        .to_string(),
                ),
                Change::Down(e) => {
                    eprintln!("warning: lost the engine on {addr}; trying again: {e}");
                    continue;
                }
            };
            self.note_link(&addr, unopenable);
        }
    }

    /// What the attached engine plays that the editor has not followed yet, taken. Left in place
    /// by not calling this, as while a Swap of the editor's own is running.
    pub fn unfollowed(&mut self) -> Option<Playing> {
        self.unfollowed.take()
    }

    /// Say why what the attached engine on `addr` plays cannot be opened, in place of the last
    /// thing said about it; `None` takes that back.
    fn note_link(&mut self, addr: &str, unopenable: Option<String>) {
        let about = format!("the engine on {addr} ");
        self.report
            .retain(|line| !(line.level == Level::Warning && line.text.starts_with(&about)));
        if let Some(why) = unopenable {
            self.push(Line::new(Level::Warning, format!("{about}{why}")));
        }
    }

    /// Nothing runs because the engine could not even be configured.
    pub fn not_started(error: impl fmt::Display) -> Self {
        Self::reported(None, None, vec![Line::new(Level::Error, error.to_string())])
    }

    /// Start the engine on the audio device `config.profile` selects, or, when that device does
    /// not open, with no device at all; and when the OSC-in port is taken, with no OSC-in. Either
    /// way the server still lists, opens and edits, and the report says what is missing and why.
    /// A engine-channel socket that does not bind is already left out by the start itself.
    pub fn start(config: EngineConfig) -> Self {
        Self::start_or_headless(config, engine::start, engine::start_headless)
    }

    /// [`start`](Self::start) through `start`, falling back to `headless` on an audio device
    /// error and to no OSC-in on an OSC-in error: the seams a test puts its stand-ins in. The kept
    /// config still names the OSC-in address, so a restart asks for it again.
    pub(crate) fn start_or_headless(
        config: EngineConfig,
        mut start: impl FnMut(EngineConfig) -> Result<RunningEngine, StartError>,
        headless: impl FnOnce(EngineConfig) -> Result<RunningEngine, StartError>,
    ) -> Self {
        let mut osc_in = None;
        let mut device = None;
        let mut engine = Self::start_with(config, |mut config| {
            let mut started = start(config.clone());
            if let Err(e @ StartError::OscIn { .. }) = &started {
                osc_in = Some(start_error(e));
                config.osc_in = None;
                started = start(config.clone());
            }
            match started {
                Err(StartError::Audio(e)) => {
                    device = Some(e);
                    headless(config)
                }
                started => started,
            }
        });
        if let Some(why) = osc_in {
            engine.push(Line::new(
                Level::Error,
                format!("OSC-in is off ({why}); instruments still open and edit"),
            ));
        }
        if let Some(e) = device {
            engine.push(Line::new(
                Level::Error,
                format!(
                    "the audio device did not open ({e}); nothing is heard, but instruments \
                     still open and edit"
                ),
            ));
        }
        engine
    }

    /// [`start`](Self::start) through `start`, the seam a test puts the headless engine in.
    pub(crate) fn start_with(
        config: EngineConfig,
        start: impl FnOnce(EngineConfig) -> Result<RunningEngine, StartError>,
    ) -> Self {
        // The server follows outside writes to the open instrument's documents, and the session
        // hears them as events: the editor has no watcher of its own.
        let config = EngineConfig {
            follow_files: true,
            ..config
        };
        let kept = Some(config.clone());
        let engine_addr = config.engine_addr.clone();
        let mut report = vec![Line::new(
            Level::Info,
            match &config.instrument {
                Instrument::Path(path) => format!("instrument: {}", path.display()),
                Instrument::Default => "instrument: <default>".to_string(),
                Instrument::Silent => "instrument: none".to_string(),
            },
        )];
        let running = match start(config) {
            Ok(running) => running,
            Err(e) => {
                report.push(Line::new(Level::Error, start_error(&e)));
                return Self::reported(None, kept, report);
            }
        };

        if let Some(target) = running.osc_out_target() {
            report.push(Line::new(
                Level::Info,
                format!("OSC-out sending to {target}"),
            ));
        }
        if let Some(addr) = running.osc_in_addr() {
            report.push(Line::new(
                Level::Info,
                format!("OSC-in listening on {addr}"),
            ));
        }
        report.push(Line::new(
            Level::Info,
            format!(
                "audio out @ {} Hz, block {}",
                running.sample_rate(),
                running.block_size()
            ),
        ));
        for w in running.warnings() {
            report.push(Line::new(Level::Warning, w.to_string()));
        }
        match (running.engine_addr(), running.channel_error()) {
            (Some(addr), _) => {
                report.push(Line::new(Level::Info, format!("engine channel on {addr}")));
            }
            (None, Some(e)) => report.push(Line::new(
                Level::Warning,
                format!(
                    "engine channel unavailable on {} ({e}); reuben-mcp cannot reach this \
                     engine{}",
                    engine_addr.as_deref().unwrap_or_default(),
                    already_running(e),
                ),
            )),
            (None, None) => {}
        }
        Self::reported(Some(running), kept, report)
    }

    /// Stop the engine and start it again through `start` with `profile`, on the instrument it is
    /// playing now — which is the last one swapped in, not necessarily the one it started on.
    /// Every port is freed and bound again, and [`report`](Self::report) becomes what this start
    /// reported, a port that would not rebind included. Does nothing when no start was ever
    /// attempted.
    ///
    /// The running engine is left alone, with an error added to the report, when the instrument's
    /// file no longer loads: stopping would trade an engine that plays for one that cannot start.
    ///
    /// `start` is [`engine::start`], or the headless engine in a test.
    pub fn restart_with(
        &mut self,
        profile: DeviceProfile,
        start: impl FnMut(EngineConfig) -> Result<RunningEngine, StartError>,
    ) {
        self.restart(profile, start, "audio output not changed");
    }

    /// [`restart_with`](Self::restart_with), saying `refused` first when the file does not load.
    /// Returns whether the engine was stopped to start again.
    fn restart(
        &mut self,
        profile: DeviceProfile,
        start: impl FnMut(EngineConfig) -> Result<RunningEngine, StartError>,
        refused: &str,
    ) -> bool {
        let Some(mut config) = self.config.clone() else {
            return false;
        };
        let mut unsourced = None;
        // Open but playing nothing, as a document that does not load is: there is no input width
        // to keep, so the restart starts on nothing and opens it again.
        let mut reopen = None;
        // What the server plays, by the name it plays it under: the file the new start reads.
        let mut playing = None;
        if let Some(running) = &self.running {
            match running.channel().get_document() {
                Ok(document) => match document.source {
                    Some(source) if document.content_hash.is_empty() => {
                        config.instrument = Instrument::Silent;
                        reopen = Some(source);
                    }
                    Some(source) => {
                        config.instrument = Instrument::Path(source.clone().into());
                        playing = Some(source);
                    }
                    None if document.content_hash.is_empty() => {
                        config.instrument = Instrument::Silent;
                    }
                    None => {
                        unsourced = Some(
                            "it is the built-in default rig, started without a file".to_string(),
                        )
                    }
                },
                Err(e) => unsourced = Some(format!("the engine did not say what it is: {e}")),
            }
        }
        if let (Some(source), Some(running)) = (&playing, &self.running) {
            if let Err(why) = loads(&running.channel(), source) {
                self.push(Line::new(
                    Level::Error,
                    format!("{refused}: {source} does not load to start again from: {why}"),
                ));
                return false;
            }
        }
        config.profile = profile;
        let offered = std::mem::take(&mut self.offered);
        self.shutdown();
        // Started on the open instrument rather than on nothing and then opened: the input stream's
        // width is fixed by what the engine starts on. A device that no longer opens falls back to
        // none, as at launch, so the server is not lost with it.
        *self = Self::start_or_headless(config, start, engine::start_headless);
        for dir in &offered {
            self.offer_dir(dir);
        }
        self.restarted = true;
        if let Some(name) = reopen {
            self.reopen(name);
        }
        if let Some(why) = unsourced {
            self.push(Line::new(
                Level::Warning,
                format!(
                    "what was playing could not be started again ({why}); playing the instrument \
                     the engine started on"
                ),
            ));
        }
        true
    }

    /// Have the restarted server open `name` again, which played nothing before the restart.
    fn reopen(&mut self, name: String) {
        let Some(channel) = self.channel() else {
            return;
        };
        let opened = match channel.open(name.clone(), None) {
            Ok(SwapOutcome::Installed(_)) => (channel.get_document())
                .is_ok_and(|document| document.source.as_deref() == Some(name.as_str())),
            _ => false,
        };
        if !opened {
            self.push(Line::new(
                Level::Warning,
                format!("{name} was not opened again after the restart"),
            ));
            return;
        }
        if let Some(config) = &mut self.config {
            config.instrument = Instrument::Path(name.clone().into());
        }
        self.name_instrument(&name);
    }

    /// Offer `dir` to the server from now on, and to the server of every restart: what it holds
    /// lists, opens and edits as the library does. Does nothing for an attached engine, whose
    /// offers are its own.
    pub fn offer_dir(&mut self, dir: &Path) {
        if self.attached.is_some() {
            return;
        }
        if !self.offered.iter().any(|offered| offered == dir) {
            self.offered.push(dir.to_path_buf());
        }
        if let Some(running) = &self.running {
            running.offer_dir(dir);
        }
    }

    /// Whether the engine restarted since the last call.
    pub fn take_restarted(&mut self) -> bool {
        std::mem::take(&mut self.restarted)
    }

    fn push(&mut self, line: Line) {
        match line.level {
            Level::Info => println!("{line}"),
            Level::Warning | Level::Error => eprintln!("{line}"),
        }
        self.report.push(line);
    }

    /// What the running engine has installed now, read without waiting; `None` when none runs.
    fn installed(&self) -> Option<Installed> {
        self.running.as_ref().map(RunningEngine::installed)
    }

    /// Bring the report and the config up to the instrument the engine has installed now, which a
    /// Swap — the editor's or `reuben-mcp`'s — may have changed since it started: the report names
    /// it, and a restart starts on it. Never waits.
    pub fn follow_installed(&mut self) {
        if self.attached.is_none() {
            match self.installed() {
                Some(Installed::Known {
                    source,
                    content_hash,
                    ..
                }) => {
                    self.plays = source.map(|source| Playing {
                        source,
                        content_hash,
                    });
                }
                Some(Installed::Busy) => {}
                Some(Installed::Poisoned) | None => self.plays = None,
            }
        }
        let Some(Installed::Known {
            source: Some(source),
            ..
        }) = self.installed()
        else {
            return;
        };
        let Some(config) = &mut self.config else {
            return;
        };
        let instrument = Instrument::Path(source.clone().into());
        if config.instrument == instrument {
            return;
        }
        config.instrument = instrument;
        self.name_instrument(&source);
    }

    /// Name `source` as the instrument in the report, in place of the one it named.
    fn name_instrument(&mut self, source: &str) {
        let named = Line::new(Level::Info, format!("instrument: {source}"));
        println!("{named}");
        match self
            .report
            .iter_mut()
            .find(|line| line.level == Level::Info && line.text.starts_with("instrument: "))
        {
            Some(line) => *line = named,
            None => self.report.insert(0, named),
        }
    }

    /// Whether the running engine's input stream fits the instrument it has installed now; `None`
    /// when no engine runs or it cannot say without waiting.
    #[cfg(test)]
    pub(crate) fn input_fits(&self) -> Option<bool> {
        let running = self.running.as_ref()?;
        match running.installed() {
            Installed::Known { input_channels, .. } => Some(input_stream_fits(
                input_channels,
                running.opened_input_channels(),
            )),
            Installed::Busy | Installed::Poisoned => None,
        }
    }

    /// Take `source`, installed as `content_hash`, as what the engine plays, as when a Swap of it
    /// answers that it landed. What the engine is next seen to play replaces it: an attached
    /// engine's link reports on its next probe whether or not that differs from its last report,
    /// since another client may have swapped back to it.
    pub fn note_installed(&mut self, source: String, content_hash: String) {
        if let Some(link) = &self.attached {
            link.forget();
        }
        self.plays = Some(Playing {
            source,
            content_hash,
        });
    }

    /// The file the engine plays, as last seen: through [`follow_installed`](Self::follow_installed)
    /// for an embedded engine, [`follow_link`](Self::follow_link) for an attached one.
    pub fn plays(&self) -> Option<&Playing> {
        self.plays.as_ref()
    }

    /// Whether a Swap holds the running engine, so what it has installed is not known yet.
    pub fn busy(&self) -> bool {
        self.installed() == Some(Installed::Busy)
    }

    /// Restart through `start` on the instrument installed now, with the same profile, when the
    /// input stream the engine opened does not fit it: the stream's width is fixed at start, so a
    /// Swap to an instrument binding other input leaves its input pipes silent, or a stream
    /// running that nothing reads. Returns whether the engine was stopped to restart, whether or
    /// not the start succeeded.
    ///
    /// A restart refused because the file does not load is not tried again for the same installed
    /// document until [`retry_fit`](Self::retry_fit). Never waits unless it restarts.
    pub fn fit_input(
        &mut self,
        start: impl FnMut(EngineConfig) -> Result<RunningEngine, StartError>,
    ) -> bool {
        let Some(running) = &self.running else {
            return false;
        };
        let opened = running.opened_input_channels();
        let Installed::Known {
            input_channels,
            content_hash,
            source,
        } = running.installed()
        else {
            return false;
        };
        if input_stream_fits(input_channels, opened) {
            return false;
        }
        let tried = (source, content_hash);
        if self.refit_tried.as_ref() == Some(&tried) {
            return false;
        }
        let Some(profile) = self.profile().cloned() else {
            return false;
        };
        let name = tried
            .0
            .clone()
            .unwrap_or_else(|| "the instrument installed".to_string());
        self.refit_tried = Some(tried);
        if !self.restart(profile, start, "input not reopened") {
            return false;
        }
        // A start that failed has already said why.
        if self.running.is_some() {
            self.push(Line::new(
                Level::Info,
                format!(
                    "restarted the engine on {name}: it binds {input_channels} input channel(s), \
                     and the input stream opened for the instrument before it has {opened}"
                ),
            ));
        }
        true
    }

    /// Let [`fit_input`](Self::fit_input) try again a restart it was refused, as when the files
    /// it could not load have changed.
    pub fn retry_fit(&mut self) {
        self.refit_tried = None;
    }

    /// What the last start was asked to play, or was since Swapped to; `None` when no start was
    /// ever attempted.
    #[cfg(test)]
    pub(crate) fn instrument(&self) -> Option<&Instrument> {
        self.config.as_ref().map(|config| &config.instrument)
    }

    /// The profile the last start was asked for; `None` when no start was ever attempted.
    pub fn profile(&self) -> Option<&DeviceProfile> {
        self.config.as_ref().map(|config| &config.profile)
    }

    fn reported(
        running: Option<RunningEngine>,
        config: Option<EngineConfig>,
        report: Vec<Line>,
    ) -> Self {
        for line in &report {
            match line.level {
                Level::Info => println!("{line}"),
                Level::Warning | Level::Error => eprintln!("{line}"),
            }
        }
        Self {
            running,
            attached: None,
            unfollowed: None,
            config,
            report,
            refit_tried: None,
            plays: None,
            offered: Vec::new(),
            restarted: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn running(&self) -> Option<&RunningEngine> {
        self.running.as_ref()
    }

    /// A channel to the running engine, or `None` when none is running — for an attached engine,
    /// when the last probe went unanswered.
    ///
    /// Live control values go through [`live`](Self::live) instead: to an attached engine every
    /// exchange is a loopback round trip, which a stopped `reuben play` holds for up to the
    /// channel's read budget.
    pub fn channel(&self) -> Option<Channel> {
        if let Some(link) = &self.attached {
            return (link.up() != Some(false)).then(|| link.channel());
        }
        self.running.as_ref().map(RunningEngine::channel)
    }

    /// Where live control values go, or `None` when no engine is running — for an attached engine,
    /// when the last probe went unanswered.
    pub fn live(&self) -> Option<Live> {
        if let Some(link) = &self.attached {
            return (link.up() != Some(false)).then(|| link.live());
        }
        self.running
            .as_ref()
            .map(|running| Live::Now(running.channel()))
    }

    /// Everything starting the engine reported, in order.
    pub fn report(&self) -> &[Line] {
        &self.report
    }

    /// Stop the engine and free every port it bound, logging its final counters as `reuben play`
    /// does. Does nothing when none is running. An attached engine is let go, not stopped.
    pub fn shutdown(&mut self) {
        self.attached = None;
        if let Some(running) = self.running.take() {
            reuben_native::diagnostics::log_snapshot(&running.shutdown());
        }
    }
}

/// Whether the instrument the server names `source` loads, as the server validates it; `Err` says
/// why not. Asked before the engine stops: the file may have stopped loading since it was
/// installed, and a start on it would leave no server at all.
fn loads(channel: &Channel, source: &str) -> Result<(), String> {
    let args = ValidateInstrument {
        source: source.to_string(),
    };
    let answer =
        call_tool(names::VALIDATE_INSTRUMENT, &args, channel).map_err(|refusal| refusal.message)?;
    let report: Report = serde_json::from_value(answer.output)
        .map_err(|e| format!("the server's report could not be read: {e}"))?;
    if report.ok {
        return Ok(());
    }
    Err(report
        .errors
        .into_iter()
        .map(|e| e.message)
        .collect::<Vec<_>>()
        .join("; "))
}

/// A [`StartError`], with the likely cause named when a port is already taken.
fn start_error(e: &StartError) -> String {
    match e {
        StartError::OscIn { error, .. } => format!("{e}{}", already_running(error)),
        e => e.to_string(),
    }
}

fn already_running(error: &io::Error) -> &'static str {
    if error.kind() == io::ErrorKind::AddrInUse {
        " — is `reuben play` or another reuben-editor already running?"
    } else {
        ""
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;

    use super::*;

    pub const HANG_SECS: u64 = 10;

    /// The repo's instrument `name`, spelled as the server names it.
    pub fn library(name: &str) -> PathBuf {
        crate::names::lexical(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../instruments")
                .join(name),
        )
    }

    /// `reuben play`'s config with ephemeral loopback ports, so parallel tests and a `reuben play`
    /// on the same machine never collide.
    pub fn config(instrument: PathBuf) -> EngineConfig {
        EngineConfig {
            osc_in: Some("127.0.0.1:0".to_string()),
            engine_addr: Some("127.0.0.1:0".to_string()),
            ..EngineConfig::new(Instrument::Path(instrument))
        }
    }

    /// [`config`] starting on nothing, the library rooted at `root`: what the standalone editor
    /// starts at launch.
    pub fn silent(root: &std::path::Path) -> EngineConfig {
        EngineConfig {
            osc_in: Some("127.0.0.1:0".to_string()),
            engine_addr: Some("127.0.0.1:0".to_string()),
            instrument_root: Some(root.to_path_buf()),
            ..EngineConfig::new(Instrument::Silent)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{SocketAddr, TcpListener, UdpSocket};
    use std::path::PathBuf;
    use std::time::Duration;

    use reuben_api::engine::{get_engine_status, open_instrument, OpenInstrument};
    use reuben_api::render::Arg;
    use reuben_mcp::EngineLink;
    use reuben_native::engine::EngineConfig;
    use reuben_native::osc;
    use reuben_native::test_support::{start_headless, within};

    use super::test_support::{config, library, HANG_SECS};
    use super::*;

    fn errors(engine: &Engine) -> Vec<&str> {
        engine
            .report()
            .iter()
            .filter(|l| l.level == Level::Error)
            .map(|l| l.text.as_str())
            .collect()
    }

    fn seed(test: &str, body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "reuben_editor_engine_{test}_{}.json",
            std::process::id()
        ));
        std::fs::write(&path, body).expect("seed a document");
        path
    }

    #[test]
    fn the_embedded_engine_follows_outside_writes() {
        let mut asked = None;
        let _ = Engine::start_with(config(library("acid-techno.json")), |config| {
            asked = Some(config.follow_files);
            Err(StartError::ChannelNeedsToken(String::new()))
        });
        assert_eq!(asked, Some(true));
    }

    #[test]
    fn a_device_that_does_not_open_starts_headless_and_says_why() {
        within(HANG_SECS, || {
            let mut engine = Engine::start_or_headless(
                super::test_support::silent(&library("")),
                |_| {
                    Err(StartError::Audio(
                        reuben_native::audio::AudioError::NoDevice,
                    ))
                },
                start_headless,
            );
            let said = errors(&engine);
            assert_eq!(said.len(), 1, "{said:?}");
            assert!(
                said[0].starts_with("the audio device did not open"),
                "{}",
                said[0]
            );
            let library = (engine.channel().expect("the server runs"))
                .library()
                .expect("the library lists without a device");
            assert!(!library.instruments.is_empty());
            engine.shutdown();
        });
    }

    #[test]
    fn the_sidecar_reaches_and_opens_on_the_embedded_engine() {
        within(HANG_SECS, || {
            let mut engine =
                Engine::start_with(config(library("acid-techno.json")), start_headless);
            assert_eq!(errors(&engine), Vec::<&str>::new());
            let engine_addr = engine
                .running()
                .and_then(RunningEngine::engine_addr)
                .expect("the engine-channel server bound");

            // The link `reuben-mcp` builds for its engine tools, over the loopback socket.
            let link = EngineLink::new(engine_addr.to_string(), None);
            let status = get_engine_status(link.channel(), env!("CARGO_PKG_VERSION"));
            assert!(status.output.reachable, "{}", status.summary);

            let target = library("groovebox.json").display().to_string();
            let opened = open_instrument(
                &OpenInstrument {
                    name: target.clone(),
                    expect: None,
                },
                link.channel(),
            )
            .expect("the open reached the engine");
            assert!(opened.output.report.report.ok, "{}", opened.summary);

            let now = link.channel().get_document().expect("read over TCP");
            assert_eq!(now.source.as_deref(), Some(target.as_str()));

            engine.shutdown();
        });
    }

    /// An OSC-in datagram reaches the render side: an `osc_out` node's port is externally
    /// addressable, so what goes in on OSC-in comes back out on OSC-out only if the render
    /// callback applied it.
    #[test]
    fn osc_in_reaches_the_render_side() {
        let path = seed(
            "echo",
            r#"{"format_version":3,"instrument":"echo",
                "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
                "nodes":[{"type":"oscillator","address":"/osc"},
                         {"type":"osc_out","address":"/echo"}]}"#,
        );
        let sink = UdpSocket::bind("127.0.0.1:0").expect("an OSC-out sink");
        sink.set_read_timeout(Some(Duration::from_secs(5)))
            .expect("a read timeout");
        let target = sink.local_addr().expect("bound").to_string();
        let instrument = path.clone();
        let received = within(HANG_SECS, move || {
            let mut engine = Engine::start_with(
                EngineConfig {
                    osc_out: Some(target),
                    ..config(instrument)
                },
                start_headless,
            );
            assert_eq!(errors(&engine), Vec::<&str>::new());
            let osc_in = engine
                .running()
                .and_then(RunningEngine::osc_in_addr)
                .expect("OSC-in bound");
            let datagram = osc::encode("/echo/in", &[Arg::F32(0.5)]).expect("encode");
            UdpSocket::bind("127.0.0.1:0")
                .expect("a sender")
                .send_to(&datagram, osc_in)
                .expect("send");
            let mut buf = [0u8; 1024];
            let n = sink.recv(&mut buf).expect("the echo came back");
            engine.shutdown();
            osc::decode(&buf[..n]).expect("decode")
        });
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].address, "/echo");
        assert_eq!(received[0].args, vec![Arg::F32(0.5)]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_path_that_does_not_load_is_reported_not_started() {
        let bad = seed("bad", r#"{"format_version":3,"instrument":"t","nodes":[{"#);
        let missing = library("does-not-exist.json");
        let reported = within(HANG_SECS, {
            let bad = bad.clone();
            move || {
                [bad, missing].map(|path| {
                    let engine = Engine::start_with(config(path), start_headless);
                    assert!(engine.running().is_none());
                    errors(&engine)
                        .into_iter()
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
            }
        });
        let [bad_errors, missing_errors] = reported;
        assert_eq!(bad_errors.len(), 1, "{bad_errors:?}");
        assert!(
            bad_errors[0].starts_with("load instrument"),
            "{bad_errors:?}"
        );
        assert_eq!(missing_errors.len(), 1, "{missing_errors:?}");
        assert!(missing_errors[0].starts_with("read "), "{missing_errors:?}");
        let _ = std::fs::remove_file(&bad);
    }

    #[test]
    fn a_taken_osc_port_is_reported_by_name() {
        let taken = UdpSocket::bind("127.0.0.1:0").expect("hold a port");
        let addr = taken.local_addr().expect("bound").to_string();
        let reported = within(HANG_SECS, {
            let addr = addr.clone();
            move || {
                let engine = Engine::start_with(
                    EngineConfig {
                        osc_in: Some(addr),
                        ..config(library("acid-techno.json"))
                    },
                    start_headless,
                );
                assert!(engine.running().is_none());
                errors(&engine)
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            }
        });
        drop(taken);
        assert_eq!(reported.len(), 1, "{reported:?}");
        assert!(reported[0].contains("OSC-in"), "{}", reported[0]);
        assert!(reported[0].contains(&addr), "{}", reported[0]);
    }

    #[test]
    fn a_taken_engine_channel_port_is_reported_by_name_and_the_engine_still_plays() {
        within(HANG_SECS, || {
            let taken = TcpListener::bind("127.0.0.1:0").expect("hold a port");
            let addr = taken.local_addr().expect("bound").to_string();
            let mut engine = Engine::start_with(
                EngineConfig {
                    engine_addr: Some(addr.clone()),
                    ..config(library("acid-techno.json"))
                },
                start_headless,
            );
            assert!(engine.running().is_some());
            let warning = engine
                .report()
                .iter()
                .find(|l| l.level == Level::Warning && l.text.contains("engine channel"))
                .expect("the engine-channel port is reported");
            assert!(warning.text.contains(&addr), "{}", warning.text);
            engine.shutdown();
        });
    }

    /// What the standalone editor starts on a machine where `reuben play`, another editor or
    /// another OSC app already holds the port: a server without that listener, and a line saying so.
    #[test]
    fn a_taken_osc_port_still_starts_a_server_and_says_why() {
        within(HANG_SECS, || {
            let taken = UdpSocket::bind("127.0.0.1:0").expect("hold a port");
            let addr = taken.local_addr().expect("bound").to_string();
            let mut engine = Engine::start_or_headless(
                EngineConfig {
                    osc_in: Some(addr.clone()),
                    ..super::test_support::silent(&library(""))
                },
                start_headless,
                |_| panic!("the headless start is only for a device that does not open"),
            );
            let running = engine.running().expect("a server runs");
            assert_eq!(running.osc_in_addr(), None);
            let said = errors(&engine);
            assert_eq!(said.len(), 1, "{said:?}");
            assert!(
                said[0].contains("OSC-in") && said[0].contains(&addr),
                "{}",
                said[0]
            );
            let library = (engine.channel().expect("the server runs"))
                .library()
                .expect("the library lists without OSC-in");
            assert!(!library.instruments.is_empty());
            assert_eq!(
                engine.config.as_ref().and_then(|c| c.osc_in.clone()),
                Some(addr),
                "a restart asks for the port again"
            );
            engine.shutdown();
            drop(taken);
        });
    }

    #[test]
    fn a_taken_osc_port_and_no_device_still_start_a_server() {
        within(HANG_SECS, || {
            let taken = UdpSocket::bind("127.0.0.1:0").expect("hold a port");
            let addr = taken.local_addr().expect("bound").to_string();
            let mut engine = Engine::start_or_headless(
                EngineConfig {
                    osc_in: Some(addr),
                    ..super::test_support::silent(&library(""))
                },
                |config| match config.osc_in {
                    Some(addr) => Err(StartError::OscIn {
                        addr,
                        error: io::Error::from(io::ErrorKind::AddrInUse),
                    }),
                    None => Err(StartError::Audio(
                        reuben_native::audio::AudioError::NoDevice,
                    )),
                },
                start_headless,
            );
            assert!(engine.running().is_some());
            assert_eq!(errors(&engine).len(), 2, "{:?}", engine.report());
            engine.shutdown();
            drop(taken);
        });
    }

    #[test]
    fn a_taken_engine_channel_port_still_starts_a_server_and_says_why() {
        within(HANG_SECS, || {
            let taken = TcpListener::bind("127.0.0.1:0").expect("hold a port");
            let addr = taken.local_addr().expect("bound").to_string();
            let mut engine = Engine::start_or_headless(
                EngineConfig {
                    engine_addr: Some(addr.clone()),
                    ..super::test_support::silent(&library(""))
                },
                start_headless,
                |_| panic!("the headless start is only for a device that does not open"),
            );
            assert!(engine.running().is_some_and(|r| r.engine_addr().is_none()));
            assert!(
                (engine.report().iter())
                    .any(|l| l.text.contains("engine channel") && l.text.contains(&addr)),
                "{:?}",
                engine.report()
            );
            let library = (engine.channel().expect("the server runs"))
                .library()
                .expect("the in-process channel serves the editor");
            assert!(!library.instruments.is_empty());
            engine.shutdown();
        });
    }

    #[test]
    fn shutdown_frees_every_port() {
        let (osc_in, engine_addr): (SocketAddr, SocketAddr) = within(HANG_SECS, || {
            let mut engine =
                Engine::start_with(config(library("acid-techno.json")), start_headless);
            let running = engine.running().expect("running");
            let ports = (
                running.osc_in_addr().expect("OSC-in bound"),
                running.engine_addr().expect("engine channel bound"),
            );
            engine.shutdown();
            assert!(engine.running().is_none());
            ports
        });
        UdpSocket::bind(osc_in).expect("the OSC-in port is free");
        TcpListener::bind(engine_addr).expect("the engine-channel port is free");
    }

    #[test]
    fn a_restart_plays_what_was_last_opened_with_the_new_profile() {
        within(HANG_SECS, || {
            let mut engine =
                Engine::start_with(config(library("acid-techno.json")), start_headless);
            let link = |engine: &Engine| {
                let addr = engine
                    .running()
                    .and_then(RunningEngine::engine_addr)
                    .expect("the engine-channel server bound");
                EngineLink::new(addr.to_string(), None)
            };

            let target = library("groovebox.json").display().to_string();
            let opened = open_instrument(
                &OpenInstrument {
                    name: target.clone(),
                    expect: None,
                },
                link(&engine).channel(),
            )
            .expect("the open reached the engine");
            assert!(opened.output.report.report.ok, "{}", opened.summary);
            let before = link(&engine)
                .channel()
                .get_document()
                .expect("read over TCP");

            let mut profile = DeviceProfile::default();
            profile.output.device = Some("Scarlett".to_string());
            let mut started_with = None;
            engine.restart_with(profile.clone(), |config| {
                started_with = Some(config.profile.clone());
                start_headless(config)
            });

            assert_eq!(errors(&engine), Vec::<&str>::new());
            assert_eq!(started_with, Some(profile.clone()));
            assert_eq!(engine.profile(), Some(&profile));
            // The sidecar reaches the restarted engine on the port it bound again.
            let after = link(&engine)
                .channel()
                .get_document()
                .expect("read over TCP");
            assert_eq!(after.source.as_deref(), Some(target.as_str()));
            assert_eq!(after.content_hash, before.content_hash);
            engine.shutdown();
        });
    }

    #[test]
    fn a_restart_onto_a_file_that_no_longer_loads_keeps_the_engine_playing() {
        let scratch = crate::session::test_support::Scratch::new("engine_restart_unloadable");
        let path = scratch.library().join("default.json");
        within(HANG_SECS, {
            let path = path.clone();
            move || {
                let mut engine = Engine::start_with(config(path.clone()), start_headless);
                assert!(engine.running().is_some());

                std::fs::write(&path, r#"{"format_version":3,"instrument":"t","nodes":[{"#)
                    .unwrap();
                engine.restart_with(DeviceProfile::default(), |_| {
                    panic!("a file that does not load is not started on")
                });
                assert!(engine.running().is_some(), "the old engine still plays");
                let refused = errors(&engine);
                assert_eq!(refused.len(), 1, "{refused:?}");
                assert!(
                    refused[0].starts_with("audio output not changed: ")
                        && refused[0].contains("does not load"),
                    "{}",
                    refused[0]
                );
                engine.shutdown();
            }
        });
    }

    #[test]
    fn a_restart_with_no_start_behind_it_does_nothing() {
        let mut idle = Engine::idle();
        idle.restart_with(DeviceProfile::default(), |_| {
            panic!("an idle editor has nothing to restart")
        });
        assert!(idle.running().is_none());
        assert!(idle.report().is_empty());

        let mut misconfigured = Engine::not_started("io-map rig.json: no such file");
        misconfigured.restart_with(DeviceProfile::default(), |_| {
            panic!("a start that was never configured has nothing to restart")
        });
        assert_eq!(misconfigured.report().len(), 1);
    }

    /// Calls [`Engine::follow_link`] until it names what the attached engine plays.
    fn await_playing(engine: &mut Engine) -> Playing {
        loop {
            engine.follow_link();
            if let Some(playing) = engine.unfollowed() {
                return playing;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn await_link(engine: &Engine, up: bool) {
        while engine.link().and_then(crate::link::Link::up) != Some(up) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn an_attached_engine_is_played_through_and_let_go_never_stopped() {
        let path = seed(
            "attach_echo",
            r#"{"format_version":3,"instrument":"echo",
                "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
                "nodes":[{"type":"oscillator","address":"/osc"},
                         {"type":"osc_out","address":"/echo"}]}"#,
        );
        let sink = UdpSocket::bind("127.0.0.1:0").expect("an OSC-out sink");
        sink.set_read_timeout(Some(Duration::from_secs(5)))
            .expect("a read timeout");
        let target = sink.local_addr().expect("bound").to_string();
        let instrument = path.clone();
        within(HANG_SECS, move || {
            // What `reuben play` runs, started by someone other than the editor.
            let played = start_headless(EngineConfig {
                osc_out: Some(target),
                ..config(instrument.clone())
            })
            .expect("the engine starts");
            let addr = played
                .engine_addr()
                .expect("the engine-channel server bound");

            let mut engine = Engine::attach(addr.to_string(), None, || {});
            let playing = await_playing(&mut engine);
            assert_eq!(playing.source, instrument.display().to_string());
            assert_eq!(
                playing.content_hash,
                played.channel().get_document().unwrap().content_hash
            );
            assert!(engine
                .report()
                .iter()
                .any(|l| l.text == format!("instrument: {}", instrument.display())));

            let channel = engine.channel().expect("the attached engine answers");
            channel
                .send(vec![reuben_api::engine::ControlMessage {
                    address: "/echo/in".to_string(),
                    args: vec![reuben_api::engine::ControlArg::F32(0.5)],
                }])
                .expect("sent");
            let mut buf = [0u8; 1024];
            let n = sink.recv(&mut buf).expect("the echo came back");
            let echoed = osc::decode(&buf[..n]).expect("decode");
            assert_eq!(echoed[0].args, vec![Arg::F32(0.5)]);

            // Nothing here owns the device, so nothing here restarts it.
            assert_eq!(engine.profile(), None);
            engine.restart_with(DeviceProfile::default(), |_| {
                panic!("an attached engine is not restarted")
            });
            assert!(!engine.fit_input(|_| panic!("an attached engine is not refit")));

            engine.shutdown();
            assert!(engine.channel().is_none());
            played.channel().ping().expect("letting go stopped nothing");

            // Losing the engine closes the channel until it answers again.
            let mut engine = Engine::attach(addr.to_string(), None, || {});
            await_link(&engine, true);
            reuben_native::diagnostics::log_snapshot(&played.shutdown());
            await_link(&engine, false);
            assert!(engine.channel().is_none());
            engine.follow_link();
            assert_eq!(engine.unfollowed(), None);
        });
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_relative_path_is_named_absolute_and_one_swapped_in_by_another_client_is_followed() {
        within(HANG_SECS, || {
            // Relative to the test's working directory, the crate's own.
            let relative = "../../instruments/default.json";
            let played =
                start_headless(config(PathBuf::from(relative))).expect("the engine starts");
            let addr = played.engine_addr().expect("bound");
            let mut engine = Engine::attach(addr.to_string(), None, || {});
            let source = PathBuf::from(await_playing(&mut engine).source);
            assert!(source.is_absolute(), "{}", source.display());
            assert!(source.is_file(), "{}", source.display());

            // An open spelled relative to the engine is published by the server's own absolute
            // name, so the editor can follow it.
            let opened = played
                .channel()
                .open("../../instruments/groovebox.json".to_string(), None)
                .expect("the open reached the engine");
            assert!(matches!(
                opened,
                reuben_api::engine::SwapOutcome::Installed(_)
            ));
            let followed = await_playing(&mut engine);
            assert_eq!(
                followed.source,
                library("groovebox.json").display().to_string()
            );
            assert!(
                !(engine.report().iter()).any(|l| l.level == Level::Warning),
                "{:?}",
                engine.report()
            );
            engine.shutdown();
            reuben_native::diagnostics::log_snapshot(&played.shutdown());
        });
    }
}

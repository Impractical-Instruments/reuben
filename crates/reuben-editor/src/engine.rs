//! The embedded engine: everything `reuben play` starts — audio out, OSC-in, the loopback structure
//! channel `reuben-mcp` dials — owned by the editor for as long as its window is open.
//!
//! Starting never panics. Whatever [`reuben_native::engine::start`] reports becomes a [`Line`] the
//! window shows, printed to the terminal as well.
//!
//! The audio streams are fixed when the engine starts, so a change of device profile is a
//! [`restart`](Engine::restart_with): the engine stops, frees its ports, and starts again on the
//! instrument it was playing. Voice state and uncommitted live values do not survive it.

use std::fmt;
use std::io;

use reuben_api::engine::Channel;
use reuben_native::engine::{self, EngineConfig, Instrument, RunningEngine, StartError};
use reuben_native::profile::DeviceProfile;

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

/// The embedded engine, when one is running, and what starting it reported.
///
/// Not `Send`, because [`RunningEngine`] is not: it lives on the UI thread that started it.
pub struct Engine {
    running: Option<RunningEngine>,
    /// What the last start was asked for, kept whether or not it succeeded, so a restart can try
    /// again with another profile. `None` when no start was ever attempted.
    config: Option<EngineConfig>,
    report: Vec<Line>,
}

impl Engine {
    /// No instrument was named: nothing runs and no port is bound.
    pub fn idle() -> Self {
        Self {
            running: None,
            config: None,
            report: Vec::new(),
        }
    }

    /// Nothing runs because the engine could not even be configured.
    pub fn not_started(error: impl fmt::Display) -> Self {
        Self::reported(None, None, vec![Line::new(Level::Error, error.to_string())])
    }

    /// Start the engine on the audio device `config.profile` selects.
    pub fn start(config: EngineConfig) -> Self {
        Self::start_with(config, engine::start)
    }

    /// [`start`](Self::start) through `start`, the seam a test puts the headless engine in.
    pub(crate) fn start_with(
        config: EngineConfig,
        start: impl FnOnce(EngineConfig) -> Result<RunningEngine, StartError>,
    ) -> Self {
        let kept = Some(config.clone());
        let structure = config.structure.clone();
        let mut report = vec![Line::new(
            Level::Info,
            match &config.instrument {
                Instrument::Path(path) => format!("instrument: {}", path.display()),
                Instrument::Default => "instrument: <default>".to_string(),
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
        match (running.structure_addr(), running.structure_error()) {
            (Some(addr), _) => {
                report.push(Line::new(
                    Level::Info,
                    format!("structure channel on {addr}"),
                ));
            }
            (None, Some(e)) => report.push(Line::new(
                Level::Warning,
                format!(
                    "structure channel unavailable on {} ({e}); reuben-mcp cannot reach this \
                     engine{}",
                    structure.as_deref().unwrap_or_default(),
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
    /// file can no longer be read: stopping would trade an engine that plays for one that cannot
    /// start.
    ///
    /// `start` is [`engine::start`], or the headless engine in a test.
    pub fn restart_with(
        &mut self,
        profile: DeviceProfile,
        start: impl FnOnce(EngineConfig) -> Result<RunningEngine, StartError>,
    ) {
        let Some(mut config) = self.config.clone() else {
            return;
        };
        let mut unsourced = None;
        if let Some(running) = &self.running {
            match running.channel().get_document() {
                Ok(document) => match document.source {
                    Some(source) => config.instrument = Instrument::Path(source.into()),
                    None => unsourced = Some("it was installed without a file".to_string()),
                },
                Err(e) => unsourced = Some(format!("the engine did not say what it is: {e}")),
            }
        }
        if let Instrument::Path(path) = &config.instrument {
            if let Err(e) = std::fs::File::open(path) {
                self.push(Line::new(
                    Level::Error,
                    format!(
                        "audio output not changed: {} cannot be read to start again from: {e}",
                        path.display()
                    ),
                ));
                return;
            }
        }
        config.profile = profile;
        self.shutdown();
        *self = Self::start_with(config, start);
        if let Some(why) = unsourced {
            self.push(Line::new(
                Level::Warning,
                format!(
                    "what was playing could not be started again ({why}); playing the instrument \
                     the engine started on"
                ),
            ));
        }
    }

    /// Start the engine as it was last asked to start, when none is running. Does nothing when
    /// one is, or when no start was ever attempted.
    pub fn resume(&mut self) {
        self.resume_with(engine::start);
    }

    /// [`resume`](Self::resume) through `start`, the seam a test puts the headless engine in.
    pub(crate) fn resume_with(
        &mut self,
        start: impl FnOnce(EngineConfig) -> Result<RunningEngine, StartError>,
    ) {
        if self.running.is_some() {
            return;
        }
        if let Some(config) = self.config.clone() {
            *self = Self::start_with(config, start);
        }
    }

    fn push(&mut self, line: Line) {
        match line.level {
            Level::Info => println!("{line}"),
            Level::Warning | Level::Error => eprintln!("{line}"),
        }
        self.report.push(line);
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
            config,
            report,
        }
    }

    #[cfg(test)]
    pub(crate) fn running(&self) -> Option<&RunningEngine> {
        self.running.as_ref()
    }

    /// A channel to the running engine, or `None` when none is running.
    ///
    /// For control traffic only on the UI thread: a send is handed to the render side without
    /// waiting, where a swap made through the same channel builds the new Engine before returning.
    pub fn channel(&self) -> Option<Channel> {
        self.running.as_ref().map(RunningEngine::channel)
    }

    /// Everything starting the engine reported, in order.
    pub fn report(&self) -> &[Line] {
        &self.report
    }

    /// Stop the engine and free every port it bound, logging its final counters as `reuben play`
    /// does. Does nothing when none is running.
    pub fn shutdown(&mut self) {
        if let Some(running) = self.running.take() {
            reuben_native::diagnostics::log_snapshot(&running.shutdown());
        }
    }
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

    pub fn library(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../instruments")
            .join(name)
    }

    /// `reuben play`'s config with ephemeral loopback ports, so parallel tests and a `reuben play`
    /// on the same machine never collide.
    pub fn config(instrument: PathBuf) -> EngineConfig {
        EngineConfig {
            osc_in: Some("127.0.0.1:0".to_string()),
            structure: Some("127.0.0.1:0".to_string()),
            ..EngineConfig::new(Instrument::Path(instrument))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{SocketAddr, TcpListener, UdpSocket};
    use std::path::PathBuf;
    use std::time::Duration;

    use reuben_api::engine::{
        get_current_instrument, get_engine_status, swap_instrument, SwapInstrument,
    };
    use reuben_api::render::Arg;
    use reuben_api::FsResolver;
    use reuben_mcp::EngineLink;
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
    fn the_sidecar_reaches_and_swaps_the_embedded_engine() {
        within(HANG_SECS, || {
            let mut engine =
                Engine::start_with(config(library("acid-techno.json")), start_headless);
            assert_eq!(errors(&engine), Vec::<&str>::new());
            let structure = engine
                .running()
                .and_then(RunningEngine::structure_addr)
                .expect("the structure server bound");

            // The link `reuben-mcp` builds for its engine tools, over the loopback socket.
            let link = EngineLink::new(structure.to_string());
            let status = get_engine_status(link.structure(), env!("CARGO_PKG_VERSION"));
            assert!(status.output.reachable, "{}", status.summary);

            let target = library("groovebox.json").display().to_string();
            let swapped = swap_instrument(
                &SwapInstrument {
                    path: target.clone(),
                    expect: None,
                },
                link.structure(),
            )
            .expect("the swap reached the engine");
            assert!(swapped.output.report.report.ok, "{}", swapped.summary);

            let store = |_: Option<&str>| FsResolver::new(library(""));
            let now = get_current_instrument(link.structure(), store).expect("read over TCP");
            assert_eq!(now.output.source.as_deref(), Some(target.as_str()));

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
    fn a_taken_structure_port_is_reported_by_name_and_the_engine_still_plays() {
        within(HANG_SECS, || {
            let taken = TcpListener::bind("127.0.0.1:0").expect("hold a port");
            let addr = taken.local_addr().expect("bound").to_string();
            let mut engine = Engine::start_with(
                EngineConfig {
                    structure: Some(addr.clone()),
                    ..config(library("acid-techno.json"))
                },
                start_headless,
            );
            assert!(engine.running().is_some());
            let warning = engine
                .report()
                .iter()
                .find(|l| l.level == Level::Warning && l.text.contains("structure channel"))
                .expect("the structure port is reported");
            assert!(warning.text.contains(&addr), "{}", warning.text);
            engine.shutdown();
        });
    }

    #[test]
    fn shutdown_frees_every_port() {
        let (osc_in, structure): (SocketAddr, SocketAddr) = within(HANG_SECS, || {
            let mut engine =
                Engine::start_with(config(library("acid-techno.json")), start_headless);
            let running = engine.running().expect("running");
            let ports = (
                running.osc_in_addr().expect("OSC-in bound"),
                running.structure_addr().expect("structure bound"),
            );
            engine.shutdown();
            assert!(engine.running().is_none());
            ports
        });
        UdpSocket::bind(osc_in).expect("the OSC-in port is free");
        TcpListener::bind(structure).expect("the structure port is free");
    }

    #[test]
    fn a_restart_plays_what_was_last_swapped_in_with_the_new_profile() {
        within(HANG_SECS, || {
            let mut engine =
                Engine::start_with(config(library("acid-techno.json")), start_headless);
            let link = |engine: &Engine| {
                let addr = engine
                    .running()
                    .and_then(RunningEngine::structure_addr)
                    .expect("the structure server bound");
                EngineLink::new(addr.to_string())
            };
            let store = |_: Option<&str>| FsResolver::new(library(""));

            let target = library("groovebox.json").display().to_string();
            let swapped = swap_instrument(
                &SwapInstrument {
                    path: target.clone(),
                    expect: None,
                },
                link(&engine).structure(),
            )
            .expect("the swap reached the engine");
            assert!(swapped.output.report.report.ok, "{}", swapped.summary);
            let before = get_current_instrument(link(&engine).structure(), store)
                .expect("read over TCP")
                .output;

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
            let after = get_current_instrument(link(&engine).structure(), store)
                .expect("read over TCP")
                .output;
            assert_eq!(after.source.as_deref(), Some(target.as_str()));
            assert_eq!(after.content_hash, before.content_hash);
            engine.shutdown();
        });
    }

    #[test]
    fn an_engine_that_did_not_start_is_started_once_its_file_loads() {
        let path = seed(
            "resume",
            r#"{"format_version":3,"instrument":"t","nodes":[{"#,
        );
        within(HANG_SECS, {
            let path = path.clone();
            move || {
                let mut engine = Engine::start_with(config(path.clone()), start_headless);
                assert!(engine.running().is_none());
                engine.resume_with(start_headless);
                assert!(engine.running().is_none(), "the file still does not load");

                std::fs::copy(library("default.json"), &path).unwrap();
                engine.resume_with(start_headless);
                assert!(engine.running().is_some());
                assert_eq!(errors(&engine), Vec::<&str>::new());

                engine.resume_with(|_| panic!("a running engine is not started again"));
                engine.shutdown();
            }
        });
        let _ = std::fs::remove_file(&path);
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
}

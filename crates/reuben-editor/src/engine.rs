//! The embedded engine: everything `reuben play` starts — audio out, OSC-in, the loopback structure
//! channel `reuben-mcp` dials — owned by the editor for as long as its window is open.
//!
//! Starting never panics. Whatever [`reuben_native::engine::start`] reports becomes a [`Line`] the
//! window shows, printed to the terminal as well.

use std::fmt;
use std::io;

use reuben_native::engine::{self, EngineConfig, Instrument, RunningEngine, StartError};

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
    report: Vec<Line>,
}

impl Engine {
    /// No instrument was named: nothing runs and no port is bound.
    pub fn idle() -> Self {
        Self {
            running: None,
            report: Vec::new(),
        }
    }

    /// Nothing runs because the engine could not even be configured.
    pub fn not_started(error: impl fmt::Display) -> Self {
        Self::reported(None, vec![Line::new(Level::Error, error.to_string())])
    }

    /// Start the engine on the audio device `config.profile` selects.
    pub fn start(config: EngineConfig) -> Self {
        Self::start_with(config, engine::start)
    }

    /// [`start`](Self::start) through `start`, the seam a test puts the headless engine in.
    fn start_with(
        config: EngineConfig,
        start: impl FnOnce(EngineConfig) -> Result<RunningEngine, StartError>,
    ) -> Self {
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
                return Self::reported(None, report);
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
        Self::reported(Some(running), report)
    }

    fn reported(running: Option<RunningEngine>, report: Vec<Line>) -> Self {
        for line in &report {
            match line.level {
                Level::Info => println!("{line}"),
                Level::Warning | Level::Error => eprintln!("{line}"),
            }
        }
        Self { running, report }
    }

    #[cfg(test)]
    fn running(&self) -> Option<&RunningEngine> {
        self.running.as_ref()
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

    use super::*;

    const HANG_SECS: u64 = 10;

    fn library(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../instruments")
            .join(name)
    }

    /// `reuben play`'s config with ephemeral loopback ports, so parallel tests and a `reuben play`
    /// on the same machine never collide.
    fn config(instrument: PathBuf) -> EngineConfig {
        EngineConfig {
            osc_in: Some("127.0.0.1:0".to_string()),
            structure: Some("127.0.0.1:0".to_string()),
            ..EngineConfig::new(Instrument::Path(instrument))
        }
    }

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
}

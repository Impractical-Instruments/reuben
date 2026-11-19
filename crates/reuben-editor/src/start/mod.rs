//! The start screen: play an instrument from the library, create a new one, or reopen a recent
//! file. Whichever is chosen, [`open`] starts the engine on it, or Swaps it in when one is
//! already running.

mod library;
mod recent;

use std::path::{Path, PathBuf};

use eframe::egui;
use reuben_api::authoring::{new_instrument, NewInstrument};
use reuben_api::FsResolver;
use reuben_native::cli::EngineFlags;
use reuben_native::engine::{EngineConfig, Instrument, RunningEngine, StartError};

use self::library::Entry;
use self::recent::Recent;
use crate::audio::AudioSettings;
use crate::config::ConfigDir;
use crate::engine::Engine;
use crate::pick;
use crate::session::Session;

/// What the engine is started with, kept from the command line for an instrument opened after
/// launch.
pub struct Launch {
    pub root: Option<PathBuf>,
    pub flags: EngineFlags,
}

/// Open the instrument at `path`: a session on its files, and `engine` playing it. A running
/// engine has the instrument Swapped in, which the session reports as for any other Swap; with
/// none running, one is started through `start`, on the stored audio choice.
///
/// `start` is [`reuben_native::engine::start`], or the headless engine in a test.
pub fn open(
    path: &Path,
    launch: &Launch,
    audio: &mut AudioSettings,
    engine: &mut Engine,
    wake: impl Fn() + Send + Sync + 'static,
    start: impl FnOnce(EngineConfig) -> Result<RunningEngine, StartError>,
) -> Session {
    let root = launch.root.as_deref();
    let mut session = Session::open(path, root, wake);
    if engine.channel().is_some() {
        session.reinstall(&|| engine.channel());
        return session;
    }
    let config = audio.engine_config(
        &launch.flags,
        Instrument::Path(path.to_path_buf()),
        launch.root.clone(),
    );
    *engine = match config {
        Ok(config) => Engine::start_with(config, start),
        Err(e) => Engine::not_started(e),
    };
    session
}

/// Write a new, valid instrument document at `path`, named for its file. Refused when a document
/// is already there.
pub fn create(path: &Path) -> Result<(), String> {
    let source = path.display().to_string();
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "untitled".to_string());
    let made = new_instrument(
        &NewInstrument {
            source: source.clone(),
            name,
        },
        &FsResolver::for_document(&source).stat_only(),
    )
    .map_err(|refusal| refusal.message)?
    .output;
    if made.written {
        return Ok(());
    }
    Err(made
        .report
        .errors
        .into_iter()
        .map(|e| e.message)
        .collect::<Vec<_>>()
        .join("; "))
}

/// The start screen's state.
pub struct Start {
    library_dir: Option<PathBuf>,
    root: Option<PathBuf>,
    /// Read when the screen is first drawn after [`show_again`](Self::show_again), not at launch,
    /// so an instrument named on the command line does not wait on reading the whole library.
    library: Option<Result<Vec<Entry>, String>>,
    recent: Recent,
    error: Option<String>,
    /// Drawn in place of the open instrument.
    shown: bool,
}

impl Start {
    pub fn new(dir: ConfigDir, root: Option<&Path>) -> Self {
        Self {
            library_dir: library::locate(root),
            root: root.map(Path::to_path_buf),
            library: None,
            recent: Recent::load(dir),
            error: None,
            shown: false,
        }
    }

    /// Put `path` first among the recent files.
    pub fn opened(&mut self, path: &Path) {
        self.recent.opened(path);
        self.error = None;
        self.shown = false;
    }

    /// Whether to draw the start screen over an open instrument.
    pub fn shown(&self) -> bool {
        self.shown
    }

    /// Draw the start screen on the next frame, with the library read again.
    pub fn show_again(&mut self) {
        self.shown = true;
        self.library = None;
    }

    /// Draw the start screen into `ui`. Returns the instrument the user chose to open. `open` is
    /// whether an instrument is open already, which the screen then offers to go back to.
    pub fn show(&mut self, ui: &mut egui::Ui, open: bool) -> Option<PathBuf> {
        let mut chosen = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("reuben-editor");
                    if open && ui.button("Back to the open instrument").clicked() {
                        self.shown = false;
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("New instrument…").clicked() {
                        if let Some(path) = pick::instrument_to_create(self.library_dir.as_deref())
                        {
                            match create(&path) {
                                Ok(()) => chosen = Some(path),
                                Err(e) => {
                                    self.error = Some(format!("{}: {e}", path.display()));
                                }
                            }
                        }
                    }
                    if ui.button("Open file…").clicked() {
                        chosen = pick::instrument_to_open(self.library_dir.as_deref());
                    }
                });
                if let Some(e) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                ui.add_space(8.0);
                ui.columns(2, |columns| {
                    self.library_ui(&mut columns[0], &mut chosen);
                    self.recent_ui(&mut columns[1], &mut chosen);
                });
            });
        chosen
    }

    fn library_ui(&mut self, ui: &mut egui::Ui, chosen: &mut Option<PathBuf>) {
        ui.strong("Library");
        let Some(dir) = &self.library_dir else {
            ui.label(
                "No library: there is no instruments/ here. Name one with --instrument-root or \
                 REUBEN_INSTRUMENT_ROOT.",
            );
            return;
        };
        ui.weak(dir.display().to_string());
        let root = self.root.as_deref();
        let library = self.library.get_or_insert_with(|| library::scan(dir, root));
        let entries = match library {
            Ok(entries) => entries,
            Err(e) => {
                ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                return;
            }
        };
        for entry in entries.iter() {
            ui.add_space(4.0);
            let button = ui.add_enabled(entry.error.is_none(), egui::Button::new(&entry.name));
            let button = match &entry.error {
                Some(e) => button.on_disabled_hover_text(e),
                None => button.on_hover_text(entry.path.display().to_string()),
            };
            if button.clicked() {
                *chosen = Some(entry.path.clone());
            }
            if let Some(description) = &entry.description {
                ui.weak(description);
            }
        }
    }

    fn recent_ui(&mut self, ui: &mut egui::Ui, chosen: &mut Option<PathBuf>) {
        ui.strong("Recent");
        if let Some(warning) = &self.recent.warning {
            ui.colored_label(ui.visuals().warn_fg_color, warning);
        }
        if self.recent.files().is_empty() {
            ui.weak("Nothing opened yet.");
        }
        for path in self.recent.files() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let exists = path.is_file();
            ui.horizontal(|ui| {
                let button = ui
                    .add_enabled(exists, egui::Button::new(name))
                    .on_hover_text(path.display().to_string())
                    .on_disabled_hover_text(format!("{} is not there", path.display()));
                if button.clicked() {
                    *chosen = Some(path.clone());
                }
                if let Some(dir) = path.parent() {
                    ui.weak(dir.display().to_string());
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use reuben_api::engine::get_current_instrument;
    use reuben_native::test_support::{start_headless, within};

    use super::*;
    use crate::audio::tests::two_devices;
    use crate::config::test_support::TempConfig;
    use crate::engine::test_support::HANG_SECS;
    use crate::session::test_support::Scratch;
    use crate::session::Status;

    /// The headless engine on ephemeral loopback ports, so parallel tests never collide.
    fn headless(mut config: EngineConfig) -> Result<RunningEngine, StartError> {
        config.osc_in = Some("127.0.0.1:0".to_string());
        config.structure = Some("127.0.0.1:0".to_string());
        start_headless(config)
    }

    fn launch() -> Launch {
        Launch {
            root: None,
            flags: EngineFlags::default(),
        }
    }

    /// The source the engine says it is playing.
    fn playing(engine: &Engine) -> Option<String> {
        let channel = engine.channel().expect("an engine is running");
        get_current_instrument(&channel, |source: Option<&str>| {
            FsResolver::for_document(source.expect("installed from a file")).stat_only()
        })
        .expect("the engine answers")
        .output
        .source
    }

    /// Poll `session` until it has Swapped what it opened in.
    fn swapped_in(session: &mut Session, engine: &Engine) {
        let deadline = Instant::now() + Duration::from_secs(HANG_SECS);
        while session.status() != &Status::Current {
            assert!(
                !matches!(session.status(), Status::Held(_)),
                "{:?}",
                session.status()
            );
            assert!(Instant::now() < deadline, "the Swap never landed");
            session.poll(Instant::now(), &|| engine.channel());
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn opening_a_library_instrument_with_no_engine_starts_one_on_it() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let mut audio = AudioSettings::load_with(temp.dir(), two_devices);
            let scratch = Scratch::new("start_library");
            let lib = scratch.library();
            let entries = library::scan(&lib, None).expect("scan");
            let groovebox = entries
                .iter()
                .find(|e| e.name == "groovebox")
                .expect("the library has groovebox");

            let mut engine = Engine::idle();
            let session = open(
                &groovebox.path,
                &launch(),
                &mut audio,
                &mut engine,
                || {},
                headless,
            );
            assert_eq!(session.status(), &Status::Current);
            assert_eq!(
                playing(&engine).as_deref(),
                Some(groovebox.path.display().to_string().as_str())
            );
            engine.shutdown();
        });
    }

    #[test]
    fn opening_with_an_engine_running_swaps_the_new_instrument_in() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let mut audio = AudioSettings::load_with(temp.dir(), two_devices);
            let scratch = Scratch::new("start_second");
            let lib = scratch.library();

            let mut engine = Engine::idle();
            let first = lib.join("default.json");
            let _first = open(&first, &launch(), &mut audio, &mut engine, || {}, headless);
            let second = lib.join("groovebox.json");
            let mut session = open(
                &second,
                &launch(),
                &mut audio,
                &mut engine,
                || {},
                |_| panic!("a running engine is not started again"),
            );
            swapped_in(&mut session, &engine);
            assert_eq!(
                playing(&engine).as_deref(),
                Some(second.display().to_string().as_str())
            );
            engine.shutdown();
        });
    }

    #[test]
    fn a_new_instrument_is_written_valid_and_plays_once_opened() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let mut audio = AudioSettings::load_with(temp.dir(), two_devices);
            let scratch = Scratch::new("start_new");
            let path = scratch.path("my-synth.json");

            create(&path).expect("created");
            let mut engine = Engine::idle();
            let session = open(&path, &launch(), &mut audio, &mut engine, || {}, headless);
            assert_eq!(session.status(), &Status::Current);
            let top = session.tree().top().read.as_ref().expect("it reads");
            assert_eq!(top.instrument, "my-synth");
            assert!(top.report.ok, "{:?}", top.report.errors);
            assert_eq!(
                playing(&engine).as_deref(),
                Some(path.display().to_string().as_str())
            );
            engine.shutdown();
        });
    }

    #[test]
    fn creating_over_an_existing_document_is_refused_and_leaves_it() {
        let scratch = Scratch::new("start_new_exists");
        let lib = scratch.library();
        let path = lib.join("default.json");
        let before = std::fs::read(&path).expect("read");
        assert!(create(&path).is_err());
        assert_eq!(std::fs::read(&path).expect("read"), before);
    }

    #[test]
    fn a_recent_file_reopens_into_the_engine() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let scratch = Scratch::new("start_recent");
            let lib = scratch.library();
            let mut start = Start::new(temp.dir(), None);
            start.opened(&lib.join("default.json"));

            let start = Start::new(temp.dir(), None);
            let path = start.recent.files()[0].clone();
            let mut audio = AudioSettings::load_with(temp.dir(), two_devices);
            let mut engine = Engine::idle();
            let session = open(&path, &launch(), &mut audio, &mut engine, || {}, headless);
            assert_eq!(session.status(), &Status::Current);
            assert_eq!(
                playing(&engine).as_deref(),
                Some(path.display().to_string().as_str())
            );
            engine.shutdown();
        });
    }

    #[test]
    fn an_instrument_that_does_not_start_is_reported() {
        let temp = TempConfig::new();
        let mut audio = AudioSettings::load_with(temp.dir(), two_devices);
        let mut engine = Engine::idle();
        let launch = Launch {
            root: None,
            flags: EngineFlags {
                io_map: Some(PathBuf::from("does-not-exist.json")),
                ..EngineFlags::default()
            },
        };
        let _session = open(
            Path::new("whatever.json"),
            &launch,
            &mut audio,
            &mut engine,
            || {},
            |_| panic!("a profile that does not load starts nothing"),
        );
        assert!(engine.channel().is_none());
        assert!(engine.report()[0]
            .text
            .starts_with("io-map does-not-exist.json"));
    }
}

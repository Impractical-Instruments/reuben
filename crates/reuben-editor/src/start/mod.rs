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
use crate::session::{Session, Status};

/// What the engine is started with, kept from the command line for an instrument opened after
/// launch.
pub struct Launch {
    pub root: Option<PathBuf>,
    pub flags: EngineFlags,
}

/// Open the instrument at `path`: a session on its files, and `engine` playing it. A running
/// engine has the instrument Swapped in, which the session reports as for any other Swap, and
/// [`follow`] then names in the engine's report; with none running, one is started through
/// `start`, on the stored audio choice. A start that fails leaves the session saying nothing
/// plays, so the next load of the files starts the engine again.
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
    if engine.channel().is_none() {
        session.reinstall(&|| None);
    }
    session
}

/// Bring `engine` up to what it has installed: its report names the instrument, and, once the
/// session's files are what it plays, an engine whose input stream does not fit them is restarted
/// on them through `start`. A restart that fails leaves the session saying nothing plays.
/// `reloaded` is whether the session read its files again since the last call, which lets a
/// restart refused for files that did not load be tried again. Called every frame.
pub fn follow(
    session: &mut Session,
    engine: &mut Engine,
    reloaded: bool,
    start: impl FnOnce(EngineConfig) -> Result<RunningEngine, StartError>,
) {
    engine.follow_installed();
    if reloaded {
        engine.retry_fit();
    }
    if session.status() != &Status::Current {
        return;
    }
    if engine.fit_input(start) && engine.channel().is_none() {
        session.reinstall(&|| None);
    }
}

/// Whether the controls panel is rebuilt from the instrument `session` just opened. Not when its
/// files do not load and an engine plays on: the panel stays the one for what is heard.
pub fn panel_follows(session: &Session, engine: &Engine) -> bool {
    !matches!(session.status(), Status::Held(_)) || engine.channel().is_none()
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

pub struct Start {
    library_dir: Result<PathBuf, String>,
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
                        if let Some(path) =
                            pick::instrument_to_create(self.library_dir.as_deref().ok())
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
                        chosen = pick::instrument_to_open(self.library_dir.as_deref().ok());
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
        let dir = match &self.library_dir {
            Ok(dir) => dir,
            Err(why) => {
                ui.label(why.as_str());
                return;
            }
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

    use reuben_api::authoring::{set_instrument_name, SetInstrumentName};
    use reuben_api::engine::{get_current_instrument, swap_instrument, SwapInstrument};
    use reuben_native::test_support::{start_headless, within};

    use super::*;
    use crate::audio::tests::two_devices;
    use crate::config::test_support::TempConfig;
    use crate::engine::test_support::HANG_SECS;
    use crate::session::test_support::Scratch;

    /// The headless engine on ephemeral loopback ports, so parallel tests never collide.
    fn headless(mut config: EngineConfig) -> Result<RunningEngine, StartError> {
        config.osc_in = Some("127.0.0.1:0".to_string());
        config.structure = Some("127.0.0.1:0".to_string());
        start_headless(config)
    }

    /// A document cut off mid-node: it does not parse.
    const BROKEN: &str = r#"{"format_version":3,"instrument":"t","nodes":[{"#;

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
            assert_eq!(engine.instrument(), Some(&Instrument::Path(first.clone())));
            swapped_in(&mut session, &engine);
            assert_eq!(
                playing(&engine).as_deref(),
                Some(second.display().to_string().as_str())
            );

            follow(&mut session, &mut engine, false, headless);
            assert_eq!(engine.instrument(), Some(&Instrument::Path(second.clone())));
            let named: Vec<_> = engine
                .report()
                .iter()
                .filter(|line| line.text.starts_with("instrument: "))
                .map(|line| line.text.clone())
                .collect();
            assert_eq!(named, [format!("instrument: {}", second.display())]);
            assert!(panel_follows(&session, &engine));
            engine.shutdown();
        });
    }

    /// Assert `engine` was restarted onto `path` with an input stream wide enough for it.
    fn restarted_with_input(engine: &Engine, path: &Path) {
        let running = engine.running().expect("the restarted engine runs");
        assert_eq!(running.opened_input_channels(), 1);
        assert_eq!(engine.input_fits(), Some(true));
        assert_eq!(
            engine.instrument(),
            Some(&Instrument::Path(path.to_path_buf()))
        );
        assert_eq!(
            playing(engine).as_deref(),
            Some(path.display().to_string().as_str())
        );
        let named = format!("instrument: {}", path.display());
        assert!(
            engine.report().iter().any(|line| line.text == named),
            "{:?}",
            engine.report()
        );
    }

    #[test]
    fn opening_an_instrument_that_reads_input_restarts_an_engine_with_no_input_stream() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let mut audio = AudioSettings::load_with(temp.dir(), two_devices);
            let scratch = Scratch::new("start_input");
            let lib = scratch.library();
            let mut engine = Engine::idle();
            let first = lib.join("default.json");
            let _first = open(&first, &launch(), &mut audio, &mut engine, || {}, headless);
            assert_eq!(
                engine.running().map(RunningEngine::opened_input_channels),
                Some(0)
            );

            let mic = lib.join("mic-space.json");
            let mut session = open(
                &mic,
                &launch(),
                &mut audio,
                &mut engine,
                || {},
                |_| panic!("a running engine is Swapped to first"),
            );
            swapped_in(&mut session, &engine);
            assert_eq!(
                engine.input_fits(),
                Some(false),
                "mic-space would read silence"
            );

            follow(&mut session, &mut engine, false, headless);
            restarted_with_input(&engine, &mic);
            assert_eq!(session.status(), &Status::Current);

            follow(&mut session, &mut engine, false, |_| {
                panic!("an engine whose input fits is not restarted again")
            });
            engine.shutdown();
        });
    }

    #[test]
    fn an_external_write_that_adds_an_input_pipe_restarts_the_engine_on_it() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let mut audio = AudioSettings::load_with(temp.dir(), two_devices);
            let scratch = Scratch::new("start_input_reload");
            let lib = scratch.library();
            let mut engine = Engine::idle();
            let path = lib.join("default.json");
            let mut session = open(&path, &launch(), &mut audio, &mut engine, || {}, headless);

            // Another program rewrites the open file as an instrument reading input channel 0.
            let text = std::fs::read_to_string(lib.join("mic-space.json")).unwrap();
            std::fs::write(&path, text).unwrap();
            let deadline = Instant::now() + Duration::from_secs(HANG_SECS);
            loop {
                session.poll(Instant::now(), &|| engine.channel());
                let reread = (session.tree().top().read.as_ref())
                    .is_ok_and(|top| top.instrument == "mic-space");
                if reread && session.status() == &Status::Current {
                    break;
                }
                assert!(Instant::now() < deadline, "{:?}", session.status());
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(engine.input_fits(), Some(false));

            follow(&mut session, &mut engine, false, headless);
            restarted_with_input(&engine, &path);
            engine.shutdown();
        });
    }

    #[test]
    fn opening_an_instrument_with_no_input_restarts_an_engine_whose_input_stream_runs() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let mut audio = AudioSettings::load_with(temp.dir(), two_devices);
            let scratch = Scratch::new("start_input_reverse");
            let lib = scratch.library();
            let mut engine = Engine::idle();
            let mic = lib.join("mic-space.json");
            let _mic = open(&mic, &launch(), &mut audio, &mut engine, || {}, headless);
            assert_eq!(
                engine.running().map(RunningEngine::opened_input_channels),
                Some(1)
            );

            let plain = lib.join("default.json");
            let mut session = open(
                &plain,
                &launch(),
                &mut audio,
                &mut engine,
                || {},
                |_| panic!("a running engine is Swapped to first"),
            );
            swapped_in(&mut session, &engine);
            follow(&mut session, &mut engine, false, headless);
            let running = engine.running().expect("the restarted engine runs");
            assert_eq!(running.opened_input_channels(), 0, "no stream runs unread");
            assert_eq!(
                playing(&engine).as_deref(),
                Some(plain.display().to_string().as_str())
            );
            engine.shutdown();
        });
    }

    /// An engine playing `default.json` of a scratch library with mic-space Swapped in over it,
    /// its input not yet fitted.
    fn mic_swapped_in(
        test: &str,
        audio: &mut AudioSettings,
    ) -> (Scratch, PathBuf, Session, Engine) {
        let scratch = Scratch::new(test);
        let lib = scratch.library();
        let mut engine = Engine::idle();
        let _first = open(
            &lib.join("default.json"),
            &launch(),
            audio,
            &mut engine,
            || {},
            headless,
        );
        let mic = lib.join("mic-space.json");
        let mut session = open(
            &mic,
            &launch(),
            audio,
            &mut engine,
            || {},
            |_| panic!("a running engine is Swapped to first"),
        );
        swapped_in(&mut session, &engine);
        assert_eq!(engine.input_fits(), Some(false));
        (scratch, mic, session, engine)
    }

    fn errors(engine: &Engine) -> Vec<String> {
        (engine.report().iter())
            .filter(|line| line.level == crate::engine::Level::Error)
            .map(|line| line.text.clone())
            .collect()
    }

    #[test]
    fn a_refused_input_restart_is_not_retried_every_frame_but_is_once_the_files_change() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let mut audio = AudioSettings::load_with(temp.dir(), two_devices);
            let (_scratch, mic, mut session, mut engine) =
                mic_swapped_in("start_refit_refused", &mut audio);
            let text = std::fs::read_to_string(&mic).unwrap();

            std::fs::write(&mic, BROKEN).unwrap();
            follow(&mut session, &mut engine, false, |_| {
                panic!("a file that does not load is not started on")
            });
            assert!(engine.running().is_some(), "mic-space plays on");
            let refused = errors(&engine);
            assert_eq!(refused.len(), 1, "{refused:?}");
            assert!(
                refused[0].starts_with("input not reopened: "),
                "{}",
                refused[0]
            );

            for _ in 0..3 {
                follow(&mut session, &mut engine, false, |_| panic!("tried again"));
            }
            assert_eq!(errors(&engine), refused, "nothing new each frame");

            std::fs::write(&mic, text).unwrap();
            follow(&mut session, &mut engine, true, headless);
            restarted_with_input(&engine, &mic);
            engine.shutdown();
        });
    }

    #[test]
    fn a_refused_input_restart_is_retried_when_another_document_is_installed() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let mut audio = AudioSettings::load_with(temp.dir(), two_devices);
            let (_scratch, mic, mut session, mut engine) =
                mic_swapped_in("start_refit_new_hash", &mut audio);
            let text = std::fs::read_to_string(&mic).unwrap();
            std::fs::write(&mic, BROKEN).unwrap();
            follow(&mut session, &mut engine, false, |_| panic!("not loadable"));
            assert_eq!(errors(&engine).len(), 1);

            // The file comes back changed, and the change is Swapped in by something other than
            // the session.
            std::fs::write(&mic, text).unwrap();
            let renamed = set_instrument_name(
                &SetInstrumentName {
                    source: mic.display().to_string(),
                    name: "mic-space-2".to_string(),
                    expect: None,
                },
                &FsResolver::for_document(&mic.display().to_string()).stat_only(),
            )
            .expect("the verb ran")
            .output;
            assert!(renamed.written, "{:?}", renamed.report);
            let swapped = swap_instrument(
                &SwapInstrument {
                    path: mic.display().to_string(),
                    expect: None,
                },
                &engine.channel().expect("running"),
            )
            .expect("the swap reached the engine");
            assert!(swapped.output.report.report.ok, "{}", swapped.summary);

            follow(&mut session, &mut engine, false, headless);
            restarted_with_input(&engine, &mic);
            engine.shutdown();
        });
    }

    #[test]
    fn a_failed_input_restart_leaves_the_session_saying_nothing_plays() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let mut audio = AudioSettings::load_with(temp.dir(), two_devices);
            let (_scratch, _mic, mut session, mut engine) =
                mic_swapped_in("start_refit_failed", &mut audio);
            // A structure address off loopback is refused before anything starts.
            follow(&mut session, &mut engine, false, |mut config| {
                config.osc_in = Some("127.0.0.1:0".to_string());
                config.structure = Some("192.0.2.1:9".to_string());
                start_headless(config)
            });
            assert!(engine.channel().is_none());
            assert_eq!(session.status(), &Status::NoEngine);
        });
    }

    #[test]
    fn the_report_names_what_a_sidecar_swapped_in_not_the_open_file() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let mut audio = AudioSettings::load_with(temp.dir(), two_devices);
            let scratch = Scratch::new("start_sidecar_swap");
            let lib = scratch.library();
            let mut engine = Engine::idle();
            let open_file = lib.join("default.json");
            let mut session = open(
                &open_file,
                &launch(),
                &mut audio,
                &mut engine,
                || {},
                headless,
            );
            let other = lib.join("groovebox.json");
            let swapped = swap_instrument(
                &SwapInstrument {
                    path: other.display().to_string(),
                    expect: None,
                },
                &engine.channel().expect("running"),
            )
            .expect("the swap reached the engine");
            assert!(swapped.output.report.report.ok, "{}", swapped.summary);

            follow(&mut session, &mut engine, false, headless);
            assert_eq!(engine.instrument(), Some(&Instrument::Path(other.clone())));
            let named = format!("instrument: {}", other.display());
            assert!(
                engine.report().iter().any(|line| line.text == named),
                "{:?}",
                engine.report()
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
        let session = open(
            &crate::engine::test_support::library("default.json"),
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
        assert_eq!(session.status(), &Status::NoEngine);
    }

    #[test]
    fn an_instrument_that_does_not_load_starts_nothing_and_says_nothing_plays() {
        let temp = TempConfig::new();
        let mut audio = AudioSettings::load_with(temp.dir(), two_devices);
        let scratch = Scratch::new("start_unloadable");
        let path = scratch.path("broken.json");
        std::fs::write(&path, BROKEN).unwrap();
        let mut engine = Engine::idle();
        let session = open(&path, &launch(), &mut audio, &mut engine, || {}, headless);
        assert!(engine.channel().is_none());
        assert!(
            !matches!(session.status(), Status::Current | Status::Swapping),
            "{:?}",
            session.status()
        );
        assert!(panel_follows(&session, &engine), "nothing else is playing");
    }

    #[test]
    fn the_instrument_root_reaches_both_the_files_and_the_engine() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let mut audio = AudioSettings::load_with(temp.dir(), two_devices);
            let scratch = Scratch::new("start_root");
            let lib = scratch.library();
            // Its voice is found only under the library root, not beside it.
            let elsewhere = scratch.path("elsewhere");
            std::fs::create_dir_all(&elsewhere).unwrap();
            let path = elsewhere.join("mine.json");
            std::fs::copy(lib.join("default.json"), &path).unwrap();
            let launch = Launch {
                root: Some(lib.clone()),
                flags: EngineFlags::default(),
            };

            let mut engine = Engine::idle();
            let session = open(
                &path,
                &launch,
                &mut audio,
                &mut engine,
                || {},
                |config| {
                    assert_eq!(config.instrument_root.as_deref(), Some(lib.as_path()));
                    headless(config)
                },
            );
            assert_eq!(session.status(), &Status::Current);
            assert!(
                session.tree().documents().iter().all(|d| d.read.is_ok()),
                "the voice resolved from the root"
            );
            assert_eq!(
                playing(&engine).as_deref(),
                Some(path.display().to_string().as_str())
            );
            engine.shutdown();
        });
    }

    #[test]
    fn a_second_instrument_that_does_not_load_keeps_the_panel_of_the_one_playing() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let mut audio = AudioSettings::load_with(temp.dir(), two_devices);
            let scratch = Scratch::new("start_second_broken");
            let lib = scratch.library();
            let mut engine = Engine::idle();
            let first = lib.join("default.json");
            let _first = open(&first, &launch(), &mut audio, &mut engine, || {}, headless);

            let broken = scratch.path("broken.json");
            std::fs::write(&broken, BROKEN).unwrap();
            let mut session = open(
                &broken,
                &launch(),
                &mut audio,
                &mut engine,
                || {},
                |_| panic!("a running engine is not started again"),
            );
            assert!(
                matches!(session.status(), Status::Held(_)),
                "{:?}",
                session.status()
            );
            assert!(!panel_follows(&session, &engine));
            follow(&mut session, &mut engine, false, headless);
            assert_eq!(engine.instrument(), Some(&Instrument::Path(first.clone())));
            assert_eq!(
                playing(&engine).as_deref(),
                Some(first.display().to_string().as_str())
            );
            engine.shutdown();
        });
    }
}

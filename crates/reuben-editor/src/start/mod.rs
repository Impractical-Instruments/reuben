//! The start screen: play an instrument from the server's library, create a new one, or reopen a
//! recent one. Whichever is chosen, [`open`] has the server open it by name.

mod library;
mod recent;

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use eframe::egui;
use reuben_api::authoring::NewInstrument;
use reuben_api::engine::wire::Library;
use reuben_api::engine::{Channel, SwapOutcome};
use reuben_api::tools::names;
use reuben_native::engine::{EngineConfig, RunningEngine, StartError};

use self::library::Entry;
use self::recent::Recent;
use crate::config::ConfigDir;
use crate::edit::{self, Verb};
use crate::engine::Engine;
use crate::pick;
use crate::session::{Session, Status};

/// What the start screen says while there is no server to list the library.
pub const NO_ENGINE: &str = "No engine is running, so there is no library to list.";

/// How long a fetch that failed stands before an engine that answers is asked again: an attached
/// editor started before its engine, or one whose engine restarted, recovers without being asked
/// every frame.
const REFETCH_AFTER: Duration = Duration::from_secs(1);

/// What the undo history says after a restart dropped it.
pub const RESTARTED: &str = "Undo history cleared: the engine restarted.";

/// Have `engine`'s server open the instrument it names `name`, unloading whatever played, and open
/// a session on its tree. A document that does not load still opens, playing nothing until it
/// does. `Err` says, for the start screen, why nothing was opened: no engine runs, the server
/// does not offer `name`, or it held or refused the open.
///
/// `wake` is called from another thread whenever the session has news off the UI thread.
pub fn open(
    name: &str,
    engine: &Engine,
    wake: impl Fn() + Send + Sync + 'static,
) -> Result<Session, String> {
    let Some(channel) = engine.channel() else {
        return Err(format!("{name} was not opened: no engine is running"));
    };
    match channel.open(name.to_string(), None) {
        // A report that is not ok either opened a document that does not load, or opened nothing.
        Ok(SwapOutcome::Installed(report)) if !report.report.ok && !is_open(&channel, name) => {
            let why: Vec<String> = (report.report.errors.into_iter())
                .map(|e| e.message)
                .collect();
            return Err(format!("{name} was not opened: {}", why.join("; ")));
        }
        Ok(SwapOutcome::Installed(_)) => {}
        Ok(SwapOutcome::Conflict(conflict)) => {
            return Err(format!(
                "{name} was not opened: what plays changed meanwhile (now {})",
                conflict.actual
            ))
        }
        Ok(SwapOutcome::Held(held)) => {
            return Err(format!(
                "{name} was not opened: the engine is holding it to ask first; {} plays on",
                held.playing.as_deref().unwrap_or("nothing")
            ))
        }
        Ok(SwapOutcome::Refused(why)) => return Err(format!("{name} was not opened: {why}")),
        Err(e) => return Err(format!("{name} was not opened: {e}")),
    }
    Ok(Session::open(name, &|| engine.channel()).with_wake(wake))
}

/// Whether `channel`'s server has `name` open.
fn is_open(channel: &Channel, name: &str) -> bool {
    (channel.get_document()).is_ok_and(|document| document.source.as_deref() == Some(name))
}

/// The second half of "Open file…", once the dialog has answered `path`: offer its directory to
/// `engine`'s server, and return the name to [`open`] it by.
pub fn offer_file(path: &Path, engine: &mut Engine) -> String {
    let name = name_of(path);
    if let Some(dir) = Path::new(&name).parent() {
        engine.offer_dir(dir);
    }
    name
}

/// The name the server gives the instrument at `path` once it plays it: the path made absolute
/// against the working directory with `.` and `..` folded by text, which is how the server lists
/// it and how an open by that spelling records it.
pub fn name_of(path: &Path) -> String {
    crate::names::lexical(path).display().to_string()
}

/// Bring `engine` up to what it has installed: its report names the instrument, and, once the
/// session's tree is what it plays, an engine whose input stream does not fit it is restarted
/// on it through `start`. A restart that fails leaves the session saying nothing plays. After any
/// restart the session's undo history is cleared, since the new server holds none of its states.
/// `reloaded` is whether the session read its tree again since the last call, which lets a
/// restart refused for files that did not load be tried again. Called every frame.
pub fn follow(
    session: &mut Session,
    engine: &mut Engine,
    reloaded: bool,
    start: impl FnMut(EngineConfig) -> Result<RunningEngine, StartError>,
) {
    engine.follow_installed();
    if reloaded {
        engine.retry_fit();
    }
    if session.status() == &Status::Current && engine.fit_input(start) && engine.channel().is_none()
    {
        session.refresh(&|| None);
    }
    if engine.take_restarted() {
        session.forget_undo(RESTARTED);
    }
}

/// Have `channel`'s server write a new, valid instrument document at `source`, named `name`.
/// Refused when a document is already there, when the server does not offer `source`, and while
/// no engine is running.
pub fn create(
    source: &str,
    name: &str,
    channel: &dyn Fn() -> Option<Channel>,
) -> Result<(), String> {
    let made = edit::call(
        channel,
        Verb::of(
            names::NEW_INSTRUMENT,
            &NewInstrument {
                source: source.to_string(),
                name: name.to_string(),
            },
        ),
    )
    .map_err(|refusal| refusal.message)?;
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

/// The source and instrument name a new instrument typed as `typed` gets in `dir`, one of the
/// server's `Library::dirs`: `<dir>/<typed>.json`, named `typed`. `None` for a name that would
/// reach outside `dir`, so the start screen cannot make one the server does not offer.
pub fn new_source(dir: &str, typed: &str) -> Option<(String, String)> {
    let typed = typed.trim();
    let name = typed.strip_suffix(".json").unwrap_or(typed);
    let outside = name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']);
    if outside {
        return None;
    }
    let source = Path::new(dir).join(format!("{name}.json"));
    Some((source.display().to_string(), name.to_string()))
}

/// What the user chose on the start screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Open the instrument the server names this.
    Open(String),
    /// Open the file the "Open file…" dialog picked, once its directory is offered.
    OpenFile(PathBuf),
}

pub struct Start {
    /// Fetched each time the screen is shown, not at launch, so an instrument named on the command
    /// line does not wait on the server reading the whole library.
    library: Option<Result<Library, String>>,
    fetching: Option<Receiver<Result<Library, String>>>,
    /// The server's `list instruments`, which a recent name must be among to open.
    offered: Option<Result<Vec<String>, String>>,
    listing: Option<Receiver<Result<Vec<String>, String>>>,
    recent: Recent,
    error: Option<String>,
    /// Drawn in place of the open instrument.
    shown: bool,
    /// Whether an engine answered when the screen was last drawn: with none, nothing is fetched.
    engine: bool,
    /// When the last fetch that failed answered; see [`REFETCH_AFTER`].
    failed_at: Option<Instant>,
    /// The new instrument's name as typed, and which of the library's dirs it goes in.
    new_name: String,
    new_dir: usize,
}

impl Start {
    pub fn new(dir: ConfigDir) -> Self {
        Self {
            library: None,
            fetching: None,
            offered: None,
            listing: None,
            recent: Recent::load(dir),
            error: None,
            shown: false,
            engine: false,
            failed_at: None,
            new_name: String::new(),
            new_dir: 0,
        }
    }

    /// Put `name` first among the recent instruments.
    pub fn opened(&mut self, name: &str) {
        self.recent.opened(name);
        self.error = None;
        self.shown = false;
    }

    /// Show the start screen saying why an instrument was not opened.
    pub fn refused(&mut self, why: String) {
        self.error = Some(why);
        self.show_again();
    }

    pub fn shown(&self) -> bool {
        self.shown
    }

    /// Draw the start screen on the next frame, with the library fetched again.
    pub fn show_again(&mut self) {
        self.shown = true;
        self.library = None;
        self.fetching = None;
        self.offered = None;
        self.listing = None;
        self.failed_at = None;
    }

    /// Take in what the server answered, and ask it whatever is not known yet.
    fn fetch(&mut self, channel: &dyn Fn() -> Option<Channel>, ctx: &egui::Context) {
        self.engine = channel().is_some();
        if let Some(answer) = self.fetching.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.fetching = None;
            if answer.is_err() {
                self.failed_at = Some(Instant::now());
            }
            self.library = Some(answer);
        }
        if let Some(answer) = self.listing.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.listing = None;
            if answer.is_err() {
                self.failed_at = Some(Instant::now());
            }
            self.offered = Some(answer);
        }
        if let (Some(at), true) = (self.failed_at, self.engine) {
            let wait = REFETCH_AFTER.saturating_sub(at.elapsed());
            if wait.is_zero() {
                self.failed_at = None;
                if matches!(self.library, Some(Err(_))) {
                    self.library = None;
                }
                if matches!(self.offered, Some(Err(_))) {
                    self.offered = None;
                }
            } else {
                ctx.request_repaint_after(wait);
            }
        }
        let wake = || {
            let ctx = ctx.clone();
            move || ctx.request_repaint()
        };
        if self.library.is_none() && self.fetching.is_none() {
            self.fetching = channel().map(|channel| library::fetch(channel, wake()));
        }
        if self.offered.is_none() && self.listing.is_none() {
            self.listing = channel().map(|channel| library::offered(channel, wake()));
        }
    }

    /// Draw the start screen into `ui`. Returns what the user chose to open. `open` is whether an
    /// instrument is open already, which the screen then offers to go back to; `embedded` is
    /// whether the engine is the editor's own, the only one "Open file…" can offer a directory to.
    /// `channel` reaches the server.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        open: bool,
        embedded: bool,
        channel: &dyn Fn() -> Option<Channel>,
    ) -> Option<Choice> {
        self.fetch(channel, ui.ctx());
        let mut chosen = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    crate::theme::logo(ui, 64.0);
                    ui.heading("reuben-editor");
                    if open && ui.button("Back to the open instrument").clicked() {
                        self.shown = false;
                    }
                });
                ui.label("Choose an instrument to play, or make a new one.");
                ui.add_space(8.0);
                self.new_ui(ui, channel, &mut chosen);
                if embedded && ui.button("Open file…").clicked() {
                    let dir = (self.library.as_ref())
                        .and_then(|library| library.as_ref().ok())
                        .and_then(|library| library.dirs.first())
                        .map(PathBuf::from);
                    chosen = pick::instrument_to_open(dir.as_deref()).map(Choice::OpenFile);
                }
                if let Some(e) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                ui.add_space(8.0);
                ui.columns(2, |columns| {
                    self.library_ui(&mut columns[0], &mut chosen);
                    self.recent_ui(&mut columns[1], embedded, &mut chosen);
                });
            });
        chosen
    }

    /// A name field and the directories the server lets a new instrument be made in.
    fn new_ui(
        &mut self,
        ui: &mut egui::Ui,
        channel: &dyn Fn() -> Option<Channel>,
        chosen: &mut Option<Choice>,
    ) {
        let dirs = match &self.library {
            Some(Ok(library)) => library.dirs.clone(),
            _ => Vec::new(),
        };
        ui.horizontal(|ui| {
            ui.label("New instrument");
            ui.add(
                egui::TextEdit::singleline(&mut self.new_name)
                    .hint_text("name")
                    .desired_width(160.0),
            );
            if self.new_dir >= dirs.len() {
                self.new_dir = 0;
            }
            egui::ComboBox::from_id_salt("new_instrument_dir")
                .selected_text(dirs.get(self.new_dir).map(String::as_str).unwrap_or("…"))
                .show_ui(ui, |ui| {
                    for (i, dir) in dirs.iter().enumerate() {
                        ui.selectable_value(&mut self.new_dir, i, dir);
                    }
                });
            let made = (dirs.get(self.new_dir)).and_then(|dir| new_source(dir, &self.new_name));
            if ui
                .add_enabled(made.is_some(), egui::Button::new("Create"))
                .clicked()
            {
                if let Some((source, name)) = made {
                    match create(&source, &name, channel) {
                        Ok(()) => {
                            self.new_name.clear();
                            *chosen = Some(Choice::Open(source));
                        }
                        Err(e) => self.error = Some(format!("{source}: {e}")),
                    }
                }
            }
        });
    }

    fn library_ui(&mut self, ui: &mut egui::Ui, chosen: &mut Option<Choice>) {
        crate::theme::section(ui, "Library");
        let library = match &self.library {
            Some(Ok(library)) => library,
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                return;
            }
            None if !self.engine => {
                ui.weak(NO_ENGINE);
                return;
            }
            None => {
                ui.weak("Reading the library…");
                return;
            }
        };
        if library.instruments.is_empty() {
            ui.weak("The server offers no instruments.");
        }
        for entry in library.instruments.iter().map(Entry::from) {
            ui.add_space(4.0);
            let button = crate::theme::choice(ui, entry.error.is_none(), &entry.title);
            let about = |ui: &mut egui::Ui| {
                if let Some(e) = &entry.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                if let Some(description) = &entry.description {
                    ui.label(description);
                }
                ui.weak(entry.name.as_str());
            };
            let button = button.on_hover_ui(about).on_disabled_hover_ui(about);
            if button.clicked() {
                *chosen = Some(Choice::Open(entry.name.clone()));
            }
        }
    }

    fn recent_ui(&mut self, ui: &mut egui::Ui, embedded: bool, chosen: &mut Option<Choice>) {
        crate::theme::section(ui, "Recent");
        if let Some(warning) = &self.recent.warning {
            ui.colored_label(ui.visuals().warn_fg_color, warning);
        }
        if self.recent.names().is_empty() {
            ui.weak("Nothing opened yet.");
        }
        let offered = match &self.offered {
            Some(Ok(offered)) => Some(offered.as_slice()),
            _ => None,
        };
        for (name, choice) in self.recent.rows(offered, embedded) {
            ui.add_space(4.0);
            let button = crate::theme::choice(ui, choice.is_some(), &library::stem(name))
                .on_hover_text(name)
                .on_disabled_hover_text(format!("the engine does not offer {name}"));
            if button.clicked() {
                *chosen = choice;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use reuben_api::authoring::{set_instrument_name, SetInstrumentName};
    use reuben_api::engine::{swap_instrument, SwapInstrument};
    use reuben_api::FsResolver;
    use reuben_native::engine::Instrument;
    use reuben_native::profile::DeviceProfile;
    use reuben_native::test_support::{start_headless, within};

    use super::*;
    use crate::config::test_support::TempConfig;
    use crate::engine::test_support::{silent, HANG_SECS};
    use crate::session::test_support::Scratch;
    use crate::session::Direction;

    /// The headless engine on ephemeral loopback ports, so parallel tests never collide.
    fn headless(mut config: EngineConfig) -> Result<RunningEngine, StartError> {
        config.osc_in = Some("127.0.0.1:0".to_string());
        config.structure = Some("127.0.0.1:0".to_string());
        start_headless(config)
    }

    /// A document cut off mid-node: it does not parse.
    const BROKEN: &str = r#"{"format_version":3,"instrument":"t","nodes":[{"#;

    /// The engine the standalone editor launches: nothing open, the library rooted at `lib`.
    fn launched(lib: &Path) -> Engine {
        Engine::start_with(silent(lib), headless)
    }

    fn name(path: &Path) -> String {
        path.display().to_string()
    }

    /// [`open`] of `path`'s name, which must open.
    fn opened(path: &Path, engine: &Engine) -> Session {
        open(&name(path), engine, || {}).expect("the server opens it")
    }

    /// The server's open instrument and the hash of what plays; empty while nothing does.
    fn playing(engine: &Engine) -> (Option<String>, String) {
        let channel = engine.channel().expect("an engine is running");
        let document = channel.get_document().expect("the engine answers");
        (document.source, document.content_hash)
    }

    fn plays(engine: &Engine) -> Option<String> {
        let (source, hash) = playing(engine);
        source.filter(|_| !hash.is_empty())
    }

    /// Assert `session` reads what it opened as playing.
    fn swapped_in(session: &mut Session, engine: &Engine) {
        session.refresh(&|| engine.channel());
        assert_eq!(session.status(), &Status::Current);
    }

    fn library_of(engine: &Engine) -> Library {
        let channel = engine.channel().expect("an engine is running");
        library::fetch(channel, || {})
            .recv_timeout(Duration::from_secs(HANG_SECS))
            .expect("the worker answers")
            .expect("the server reads its library")
    }

    #[test]
    fn opening_by_name_plays_it() {
        within(HANG_SECS, || {
            let scratch = Scratch::new("start_by_name");
            let lib = scratch.library();
            let mut engine = launched(&lib);
            assert_eq!(playing(&engine), (None, String::new()));
            let groovebox = (library_of(&engine).instruments.iter())
                .map(Entry::from)
                .find(|e| e.title == "groovebox")
                .expect("the library has groovebox");

            let session = open(&groovebox.name, &engine, || {}).expect("opened");
            assert_eq!(session.status(), &Status::Current);
            assert_eq!(session.top(), groovebox.name);
            assert_eq!(plays(&engine), Some(groovebox.name.clone()));
            engine.shutdown();
        });
    }

    #[test]
    fn a_name_the_server_does_not_offer_is_refused_on_the_start_screen() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let scratch = Scratch::new("start_not_offered");
            let lib = scratch.library();
            let elsewhere = scratch.path("elsewhere");
            std::fs::create_dir_all(&elsewhere).unwrap();
            let outside = elsewhere.join("mine.json");
            std::fs::copy(lib.join("default.json"), &outside).unwrap();
            let mut engine = launched(&lib);
            let first = opened(&lib.join("groovebox.json"), &engine);

            let why = open(&name(&outside), &engine, || {})
                .err()
                .expect("refused");
            assert!(why.contains("not offered"), "{why}");
            assert_eq!(
                plays(&engine),
                Some(first.top().to_string()),
                "what played plays on"
            );

            let mut start = Start::new(temp.dir());
            start.refused(why.clone());
            assert!(start.shown());
            assert_eq!(start.error, Some(why));
            engine.shutdown();
        });
    }

    #[test]
    fn opening_with_no_engine_running_is_refused() {
        let engine = Engine::not_started("io-map does-not-exist.json: no such file");
        let why = open("/lib/default.json", &engine, || {})
            .err()
            .expect("refused");
        assert!(why.contains("no engine is running"), "{why}");
    }

    #[test]
    fn opening_with_an_engine_running_swaps_the_new_instrument_in() {
        within(HANG_SECS, || {
            let scratch = Scratch::new("start_second");
            let lib = scratch.library();
            let mut engine = launched(&lib);
            let first = lib.join("default.json");
            let _first = opened(&first, &engine);
            let second = lib.join("groovebox.json");
            let mut session = opened(&second, &engine);
            swapped_in(&mut session, &engine);
            assert_eq!(plays(&engine), Some(name(&second)));

            follow(&mut session, &mut engine, false, |_| {
                panic!("a running engine is not started again")
            });
            assert_eq!(engine.instrument(), Some(&Instrument::Path(second.clone())));
            let named: Vec<_> = engine
                .report()
                .iter()
                .filter(|line| line.text.starts_with("instrument: "))
                .map(|line| line.text.clone())
                .collect();
            assert_eq!(named, [format!("instrument: {}", second.display())]);
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
        assert_eq!(plays(engine), Some(name(path)));
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
            let scratch = Scratch::new("start_input");
            let lib = scratch.library();
            let mut engine = launched(&lib);
            let _first = opened(&lib.join("default.json"), &engine);
            assert_eq!(
                engine.running().map(RunningEngine::opened_input_channels),
                Some(0)
            );

            let mic = lib.join("mic-space.json");
            let mut session = opened(&mic, &engine);
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
            let scratch = Scratch::new("start_input_reload");
            let lib = scratch.library();
            let mut engine = launched(&lib);
            let path = lib.join("default.json");
            let mut session = opened(&path, &engine);

            // Another program rewrites the open file as an instrument reading input channel 0.
            let text = std::fs::read_to_string(lib.join("mic-space.json")).unwrap();
            std::fs::write(&path, text).unwrap();
            let deadline = Instant::now() + Duration::from_secs(HANG_SECS);
            loop {
                session.refresh(&|| engine.channel());
                let reread = (session.tree().top().read.as_ref())
                    .is_ok_and(|top| top.instrument == "mic-space");
                // The engine follows the write itself; the session reads what it follows.
                let installed = engine.input_fits() == Some(false);
                if reread && installed && session.status() == &Status::Current {
                    break;
                }
                assert!(Instant::now() < deadline, "{:?}", session.status());
                std::thread::sleep(Duration::from_millis(5));
            }

            follow(&mut session, &mut engine, false, headless);
            restarted_with_input(&engine, &path);
            engine.shutdown();
        });
    }

    #[test]
    fn opening_an_instrument_with_no_input_restarts_an_engine_whose_input_stream_runs() {
        within(HANG_SECS, || {
            let scratch = Scratch::new("start_input_reverse");
            let lib = scratch.library();
            let mut engine = launched(&lib);
            let mic = lib.join("mic-space.json");
            let mut mic_session = opened(&mic, &engine);
            follow(&mut mic_session, &mut engine, false, headless);
            assert_eq!(
                engine.running().map(RunningEngine::opened_input_channels),
                Some(1)
            );

            let plain = lib.join("default.json");
            let mut session = opened(&plain, &engine);
            swapped_in(&mut session, &engine);
            follow(&mut session, &mut engine, false, headless);
            let running = engine.running().expect("the restarted engine runs");
            assert_eq!(running.opened_input_channels(), 0, "no stream runs unread");
            assert_eq!(plays(&engine), Some(name(&plain)));
            engine.shutdown();
        });
    }

    /// An engine with `default.json` of a scratch library opened, then mic-space opened over it,
    /// its input not yet fitted.
    fn mic_swapped_in(test: &str) -> (Scratch, PathBuf, Session, Engine) {
        let scratch = Scratch::new(test);
        let lib = scratch.library();
        let engine = launched(&lib);
        let _first = opened(&lib.join("default.json"), &engine);
        let mic = lib.join("mic-space.json");
        let mut session = opened(&mic, &engine);
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
            let (_scratch, mic, mut session, mut engine) = mic_swapped_in("start_refit_refused");
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
            let (_scratch, mic, mut session, mut engine) = mic_swapped_in("start_refit_new_hash");
            let text = std::fs::read_to_string(&mic).unwrap();
            std::fs::write(&mic, BROKEN).unwrap();
            follow(&mut session, &mut engine, false, |_| panic!("not loadable"));
            assert_eq!(errors(&engine).len(), 1);

            // The file comes back changed, and the change is Swapped in by something other than
            // the session.
            std::fs::write(&mic, text).unwrap();
            let renamed = set_instrument_name(
                &SetInstrumentName {
                    source: name(&mic),
                    name: "mic-space-2".to_string(),
                    expect: None,
                },
                &FsResolver::for_document(&name(&mic)).stat_only(),
            )
            .expect("the verb ran")
            .output;
            assert!(renamed.written, "{:?}", renamed.report);
            let swapped = swap_instrument(
                &SwapInstrument {
                    path: name(&mic),
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
            let (_scratch, _mic, mut session, mut engine) = mic_swapped_in("start_refit_failed");
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
            let scratch = Scratch::new("start_sidecar_swap");
            let lib = scratch.library();
            let mut engine = launched(&lib);
            let mut session = opened(&lib.join("default.json"), &engine);
            let other = lib.join("groovebox.json");
            let swapped = swap_instrument(
                &SwapInstrument {
                    path: name(&other),
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

    /// Rename the instrument `session` has open, through the server, so its undo has a step.
    fn rename(session: &mut Session, engine: &Engine) {
        let source = session.top().to_string();
        let channel = || engine.channel();
        let edit = crate::edit::write(session, &channel, &source, |expect| {
            Verb::of(
                names::SET_INSTRUMENT_NAME,
                &SetInstrumentName {
                    source: source.clone(),
                    name: "renamed".to_string(),
                    expect,
                },
            )
        })
        .expect("the verb ran");
        assert!(edit.written, "{:?}", edit.report);
        assert!(session.undo().next(Direction::Undo).is_some());
    }

    #[test]
    fn an_undo_after_opening_another_instrument_does_not_touch_the_first() {
        within(HANG_SECS, || {
            let scratch = Scratch::new("start_undo");
            let lib = scratch.library();
            let mut engine = launched(&lib);
            let first = lib.join("default.json");
            let mut session = opened(&first, &engine);
            rename(&mut session, &engine);
            let edited = std::fs::read(&first).unwrap();

            let mut session = opened(&lib.join("groovebox.json"), &engine);
            assert!(session.undo().next(Direction::Undo).is_none());
            let channel = || engine.channel();
            let restored = crate::edit::restore(&mut session, &channel, Direction::Undo);
            assert!(
                matches!(restored, crate::edit::Restored::Nothing),
                "{restored:?}"
            );
            assert_eq!(
                std::fs::read(&first).unwrap(),
                edited,
                "the first file is left as it is"
            );
            engine.shutdown();
        });
    }

    #[test]
    fn a_restart_clears_undo_and_says_so() {
        within(HANG_SECS, || {
            let scratch = Scratch::new("start_restart_undo");
            let lib = scratch.library();
            let elsewhere = scratch.path("elsewhere");
            std::fs::create_dir_all(&elsewhere).unwrap();
            let mut engine = launched(&lib);
            engine.offer_dir(&elsewhere);
            let top = lib.join("default.json");
            let mut session = opened(&top, &engine);
            rename(&mut session, &engine);

            engine.restart_with(DeviceProfile::default(), headless);
            follow(&mut session, &mut engine, false, |_| {
                panic!("a restarted engine whose input fits is not restarted again")
            });
            assert!(session.undo().next(Direction::Undo).is_none());
            assert_eq!(session.undo().note(), Some(RESTARTED));
            assert_eq!(
                plays(&engine),
                Some(name(&top)),
                "the open name plays again"
            );
            assert!(
                library_of(&engine).dirs.contains(&name(&elsewhere)),
                "the restarted server offers what the last one did"
            );
            engine.shutdown();
        });
    }

    #[test]
    fn a_restart_with_a_document_that_does_not_load_open_opens_it_again() {
        within(HANG_SECS, || {
            let scratch = Scratch::new("start_restart_broken");
            let lib = scratch.library();
            let broken = lib.join("broken.json");
            std::fs::write(&broken, BROKEN).unwrap();
            let mut engine = launched(&lib);
            let _session = opened(&broken, &engine);

            engine.restart_with(DeviceProfile::default(), headless);
            assert!(engine.running().is_some(), "{:?}", engine.report());
            assert_eq!(errors(&engine), Vec::<String>::new());
            assert_eq!(playing(&engine), (Some(name(&broken)), String::new()));
            engine.shutdown();
        });
    }

    #[test]
    fn an_engine_started_without_its_device_survives_an_input_refit() {
        within(HANG_SECS, || {
            let scratch = Scratch::new("start_refit_no_device");
            let lib = scratch.library();
            let no_device = |_: EngineConfig| -> Result<RunningEngine, StartError> {
                Err(StartError::Audio(
                    reuben_native::audio::AudioError::NoDevice,
                ))
            };
            let mut engine = Engine::start_or_headless(silent(&lib), no_device, headless);
            assert!(engine.running().is_some());
            let mic = lib.join("mic-space.json");
            let mut session = opened(&mic, &engine);
            swapped_in(&mut session, &engine);
            assert_eq!(engine.input_fits(), Some(false));

            follow(&mut session, &mut engine, false, no_device);
            assert!(engine.channel().is_some(), "{:?}", engine.report());
            assert_eq!(plays(&engine), Some(name(&mic)));
            assert!(library_of(&engine).instruments.len() > 1);
            engine.shutdown();
        });
    }

    #[test]
    fn a_name_typed_for_a_new_instrument_stays_in_the_dir_chosen() {
        assert_eq!(
            new_source("/lib", "pad"),
            Some(("/lib/pad.json".to_string(), "pad".to_string()))
        );
        assert_eq!(
            new_source("/lib", " pad.json "),
            Some(("/lib/pad.json".to_string(), "pad".to_string()))
        );
        for outside in [
            "", "  ", ".", "..", "../pad", "sub/pad", "sub\\pad", "/etc/pad",
        ] {
            assert_eq!(new_source("/lib", outside), None, "{outside:?}");
        }
    }

    #[test]
    fn a_new_instrument_is_created_in_an_offered_dir_and_opened() {
        within(HANG_SECS, || {
            let scratch = Scratch::new("start_new");
            let lib = scratch.library();
            // A cold start: nothing is open.
            let mut engine = launched(&lib);
            let dirs = library_of(&engine).dirs;
            let (source, made) = new_source(&dirs[0], "my-synth").expect("a name in the dir");

            create(&source, &made, &|| engine.channel()).expect("created");
            let mut session = open(&source, &engine, || {}).expect("opened");
            swapped_in(&mut session, &engine);
            let top = session.tree().top().read.as_ref().expect("it reads");
            assert_eq!(top.instrument, "my-synth");
            assert!(top.report.ok, "{:?}", top.report.errors);
            assert_eq!(plays(&engine), Some(source));
            engine.shutdown();
        });
    }

    #[test]
    fn creating_over_an_existing_document_is_refused_and_leaves_it() {
        within(HANG_SECS, || {
            let scratch = Scratch::new("start_new_exists");
            let lib = scratch.library();
            let mut engine = launched(&lib);
            let path = lib.join("default.json");
            let before = std::fs::read(&path).expect("read");
            assert!(create(&name(&path), "default", &|| engine.channel()).is_err());
            assert_eq!(std::fs::read(&path).expect("read"), before);
            engine.shutdown();
        });
    }

    #[test]
    fn creating_with_no_engine_running_is_refused_and_writes_nothing() {
        let scratch = Scratch::new("start_new_no_engine");
        let path = scratch.path("my-synth.json");
        assert!(create(&name(&path), "my-synth", &|| None).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn a_recent_file_reopens_into_the_engine() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let scratch = Scratch::new("start_recent");
            let lib = scratch.library();
            let mut start = Start::new(temp.dir());
            start.opened(&name(&lib.join("default.json")));

            let start = Start::new(temp.dir());
            let recent = start.recent.names()[0].clone();
            let mut engine = launched(&lib);
            let session = open(&recent, &engine, || {}).expect("opened");
            assert_eq!(session.status(), &Status::Current);
            assert_eq!(plays(&engine), Some(recent));
            engine.shutdown();
        });
    }

    #[test]
    fn an_instrument_that_does_not_load_opens_silent_and_plays_once_fixed() {
        within(HANG_SECS, || {
            let scratch = Scratch::new("start_unloadable");
            let lib = scratch.library();
            let path = lib.join("broken.json");
            std::fs::write(&path, BROKEN).unwrap();
            let mut engine = launched(&lib);
            let mut session = opened(&path, &engine);
            assert!(
                matches!(session.status(), Status::Held(_)),
                "{:?}",
                session.status()
            );
            assert_eq!(playing(&engine), (Some(name(&path)), String::new()));

            // Fixed outside the editor: the server follows the write and plays it.
            std::fs::copy(lib.join("default.json"), &path).unwrap();
            let deadline = Instant::now() + Duration::from_secs(HANG_SECS);
            while plays(&engine).is_none() {
                assert!(Instant::now() < deadline, "the fixed file never played");
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(plays(&engine), Some(name(&path)));
            swapped_in(&mut session, &engine);
            engine.shutdown();
        });
    }

    #[test]
    fn open_file_offers_its_directory_then_opens_it() {
        within(HANG_SECS, || {
            let scratch = Scratch::new("start_open_file");
            let lib = scratch.library();
            let elsewhere = scratch.path("elsewhere");
            std::fs::create_dir_all(&elsewhere).unwrap();
            let path = elsewhere.join("mine.json");
            std::fs::copy(lib.join("groovebox.json"), &path).unwrap();
            let mut engine = launched(&lib);
            assert!(
                open(&name(&path), &engine, || {}).is_err(),
                "not offered yet"
            );

            let offered = offer_file(&path, &mut engine);
            assert_eq!(offered, name(&path));
            let session = open(&offered, &engine, || {}).expect("opened");
            assert_eq!(session.status(), &Status::Current);
            assert_eq!(plays(&engine), Some(offered));
            assert!(library_of(&engine).dirs.contains(&name(&elsewhere)));
            engine.shutdown();
        });
    }

    #[test]
    fn a_dot_dot_path_is_named_as_the_library_and_the_server_name_it() {
        within(HANG_SECS, || {
            let scratch = Scratch::new("start_dot_dot");
            let lib = scratch.library();
            let mut engine = launched(&lib);
            let path = lib.join("voices").join("..").join("groovebox.json");
            let offered = offer_file(&path, &mut engine);
            let listed = (library_of(&engine).instruments.into_iter())
                .map(|entry| entry.name)
                .find(|listed| listed.ends_with("groovebox.json"))
                .expect("the library lists groovebox");
            assert_eq!(offered, listed);
            let session = open(&offered, &engine, || {}).expect("opened");
            assert_eq!(session.top(), listed);
            assert_eq!(plays(&engine), Some(listed));
            engine.shutdown();
        });
    }

    /// An attached editor started before the engine: the first fetch fails, and the screen asks
    /// again once an engine answers, though not more than once per [`REFETCH_AFTER`].
    #[test]
    fn a_failed_fetch_is_asked_again_once_an_engine_answers() {
        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let scratch = Scratch::new("start_refetch");
            let lib = scratch.library();
            let ctx = egui::Context::default();
            let mut start = Start::new(temp.dir());
            let gone = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = gone.local_addr().unwrap().to_string();
            drop(gone);
            let dead = move || {
                Some(Channel::new(reuben_api::engine::TcpTransport::new(
                    addr.clone(),
                    Duration::from_millis(200),
                )))
            };
            let deadline = Instant::now() + Duration::from_secs(HANG_SECS);
            while !matches!(start.library, Some(Err(_))) || !matches!(start.offered, Some(Err(_))) {
                assert!(Instant::now() < deadline, "the fetch never failed");
                start.fetch(&dead, &ctx);
                std::thread::sleep(Duration::from_millis(5));
            }

            let mut engine = launched(&lib);
            let live = || engine.channel();
            start.fetch(&live, &ctx);
            assert!(matches!(start.library, Some(Err(_))), "asked again at once");
            start.failed_at = start.failed_at.map(|at| at - REFETCH_AFTER);
            while !matches!(start.library, Some(Ok(_))) || !matches!(start.offered, Some(Ok(_))) {
                assert!(Instant::now() < deadline, "never fetched again");
                start.fetch(&live, &ctx);
                std::thread::sleep(Duration::from_millis(5));
            }
            engine.shutdown();
        });
    }

    #[test]
    fn the_instrument_root_reaches_both_the_files_and_the_engine() {
        within(HANG_SECS, || {
            let scratch = Scratch::new("start_root");
            let lib = scratch.library();
            // Its voice is found only under the library root, not beside it.
            let elsewhere = scratch.path("elsewhere");
            std::fs::create_dir_all(&elsewhere).unwrap();
            let path = elsewhere.join("mine.json");
            std::fs::copy(lib.join("default.json"), &path).unwrap();

            let mut engine = Engine::start_with(silent(&lib), |config| {
                assert_eq!(config.instrument_root.as_deref(), Some(lib.as_path()));
                headless(config)
            });
            let offered = offer_file(&path, &mut engine);
            let session = open(&offered, &engine, || {}).expect("opened");
            assert_eq!(session.status(), &Status::Current);
            assert!(
                session.tree().documents().iter().all(|d| d.read.is_ok()),
                "the voice resolved from the root"
            );
            assert_eq!(plays(&engine), Some(name(&path)));
            engine.shutdown();
        });
    }

    #[test]
    fn a_second_instrument_that_does_not_load_is_opened_with_nothing_playing() {
        within(HANG_SECS, || {
            let scratch = Scratch::new("start_second_broken");
            let lib = scratch.library();
            let mut engine = launched(&lib);
            let _first = opened(&lib.join("default.json"), &engine);

            let broken = lib.join("broken.json");
            std::fs::write(&broken, BROKEN).unwrap();
            let mut session = opened(&broken, &engine);
            assert!(
                matches!(session.status(), Status::Held(_)),
                "{:?}",
                session.status()
            );
            assert_eq!(playing(&engine), (Some(name(&broken)), String::new()));
            follow(&mut session, &mut engine, false, |_| {
                panic!("nothing restarts on a document that does not load")
            });
            engine.shutdown();
        });
    }
}

//! A Swap `reuben-mcp` asks the embedded engine for, of an instrument other than the one open
//! here: held rather than installed, so a background agent does not replace what the user is
//! working on, and put to the user to accept or decline.
//!
//! A Swap of the open top-level file is not held: it installs what the file watcher would install
//! anyway. Any other source is, a voice or subpatch file inside the open instrument's tree
//! included, since installing it would replace the top-level instrument.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use eframe::egui;
use reuben_native::structure::SwapHold;

/// How long the prompt's buttons ignore clicks after the source it asks about appears or
/// changes, so a click the user aimed at what was there before does not answer it.
const ARMING: Duration = Duration::from_millis(600);

/// The open instrument and the one Swap waiting on the user, shared between the engine's
/// structure server, which holds a Swap, and the UI thread, which asks about it.
#[derive(Clone)]
pub struct HeldSwaps {
    inner: Arc<Inner>,
}

struct Inner {
    /// One lock for both, so a Swap judged against one open file cannot leave a question about
    /// the file opened meanwhile.
    state: Mutex<State>,
    wake: Box<dyn Fn() + Send + Sync>,
}

#[derive(Default)]
struct State {
    /// The open top-level file, [`canonical`]. `None` while nothing is open, when nothing
    /// is held.
    open: Option<PathBuf>,
    /// A later held Swap replaces it.
    pending: Option<Asked>,
}

/// A held Swap the user is being asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    /// The engine's name for it, which it plays under once opened, so the agent sees the source
    /// it asked for.
    pub path: PathBuf,
    canonical: PathBuf,
    at: Instant,
}

impl HeldSwaps {
    /// `wake` is called from the structure server's thread when a Swap is held, so the prompt is
    /// drawn without waiting for input.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::default()),
                wake: Box::new(wake),
            }),
        }
    }

    /// The decision to start the engine with.
    pub fn swap_hold(&self) -> SwapHold {
        let held = self.clone();
        SwapHold::new(move |path| held.hold(Path::new(path)))
    }

    /// `top` is the open instrument now. A prompt for that same file has nothing left to ask.
    pub fn opened(&self, top: &Path) {
        let top = canonical(top);
        let mut state = self.state();
        if state.pending.as_ref().is_some_and(|p| p.canonical == top) {
            state.pending = None;
        }
        state.open = Some(top);
    }

    /// Nothing is open now, as after an open the server refused with nothing open before it: no
    /// Swap is held.
    pub fn closed(&self) {
        self.state().open = None;
    }

    /// The Swap the user is being asked about, if any.
    pub fn pending(&self) -> Option<Asked> {
        self.state().pending.clone()
    }

    /// The user chose to open `shown`. Returns the name to open it by, as the client asked for
    /// it, unless another held Swap has replaced the one they were shown.
    pub fn accept(&self, shown: &Asked) -> Option<String> {
        let mut state = self.state();
        if state.pending.as_ref() != Some(shown) {
            return None;
        }
        state
            .pending
            .take()
            .map(|asked| asked.path.display().to_string())
    }

    /// The user kept what is open rather than `shown`: nothing changes. A Swap held since they
    /// were shown it is still asked about.
    pub fn decline(&self, shown: &Asked) {
        let mut state = self.state();
        if state.pending.as_ref() == Some(shown) {
            state.pending = None;
        }
    }

    /// Runs under the engine's Coordinator lock, so it records the question and returns; the
    /// user's answer comes later on the UI thread.
    fn hold(&self, path: &Path) -> bool {
        let asked = canonical(path);
        {
            let mut state = self.state();
            match &state.open {
                Some(open) if *open != asked => {}
                _ => return false,
            }
            state.pending = Some(Asked {
                path: path.to_path_buf(),
                canonical: asked,
                at: Instant::now(),
            });
        }
        (self.inner.wake)();
        true
    }

    /// The cell holds plain values that a panic cannot leave half-written.
    fn state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// `path` as the server names it: absolute, with `.` and `..` folded by text, so two spellings of
/// one name compare equal without asking the filesystem, which for an attached editor is not the
/// server's.
pub fn canonical(path: &Path) -> PathBuf {
    crate::names::lexical(path)
}

/// Draw the prompt while a Swap is held, floating over the bottom-right corner of `within` so
/// nothing under it moves when it appears. It takes no focus, and its buttons ignore clicks for [`ARMING`]
/// after what it asks about changes, so a gesture under way carries on until the user turns to
/// it. Returns the name the user chose to open.
pub fn show(ctx: &egui::Context, held: &HeldSwaps, within: egui::Rect) -> Option<String> {
    let asked = held.pending()?;
    let armed = asked.at.elapsed() >= ARMING;
    if !armed {
        ctx.request_repaint_after(ARMING.saturating_sub(asked.at.elapsed()));
    }
    let mut answer = None;
    egui::Area::new(egui::Id::new("held_swap"))
        .pivot(egui::Align2::RIGHT_BOTTOM)
        .fixed_pos(within.right_bottom() - egui::vec2(16.0, 16.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(360.0);
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("An agent asked to play {}.", name(&asked.path)),
                )
                .on_hover_text(asked.path.display().to_string());
                ui.label("What is playing keeps playing until you choose.");
                ui.horizontal(|ui| {
                    let open = egui::Button::new(format!("Open {}", name(&asked.path)));
                    if ui.add_enabled(armed, open).clicked() {
                        answer = held.accept(&asked);
                    }
                    if ui
                        .add_enabled(armed, egui::Button::new("Keep what is open"))
                        .clicked()
                    {
                        held.decline(&asked);
                    }
                });
            });
        });
    answer
}

fn name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use reuben_api::engine::{open_instrument, OpenInstrument, SwapResult};
    use reuben_mcp::EngineLink;
    use reuben_native::engine::{EngineConfig, RunningEngine, StartError};
    use reuben_native::test_support::{start_headless, within};

    use super::*;
    use crate::engine::test_support::{silent, HANG_SECS};
    use crate::engine::Engine;
    use crate::session::test_support::Scratch;
    use crate::session::{Session, Status};
    use crate::start;

    fn headless(mut config: EngineConfig) -> Result<RunningEngine, StartError> {
        config.osc_in = Some("127.0.0.1:0".to_string());
        config.structure = Some("127.0.0.1:0".to_string());
        start_headless(config)
    }

    /// The editor on a scratch library with `instrument` open, and `reuben-mcp`'s link to its
    /// engine over the loopback socket.
    struct Editor {
        lib: PathBuf,
        held: HeldSwaps,
        wakes: Arc<AtomicUsize>,
        engine: Engine,
        session: Session,
        link: EngineLink,
        _scratch: Scratch,
    }

    impl Editor {
        fn open(test: &str, instrument: &str) -> Self {
            let scratch = Scratch::new(test);
            let lib = scratch.library();
            let wakes = Arc::new(AtomicUsize::new(0));
            let held = HeldSwaps::new({
                let wakes = Arc::clone(&wakes);
                move || {
                    wakes.fetch_add(1, Ordering::SeqCst);
                }
            });
            let engine = Engine::start_with(
                EngineConfig {
                    swap_hold: Some(held.swap_hold()),
                    ..silent(&lib)
                },
                headless,
            );
            let top = lib.join(instrument).display().to_string();
            let mut editor = Self {
                lib,
                held,
                wakes,
                engine,
                session: Session::open(&top, &|| None),
                link: EngineLink::default(),
                _scratch: scratch,
            };
            editor.open_file(&top);
            let structure = editor
                .engine
                .running()
                .and_then(RunningEngine::structure_addr)
                .expect("the structure server bound");
            editor.link = EngineLink::new(structure.to_string()).expect("a loopback address");
            editor
        }

        /// What the editor does to open an instrument by name, from the start screen or from the
        /// prompt, in the order it does it.
        fn open_file(&mut self, name: &str) {
            self.held.opened(Path::new(name));
            self.session = start::open(name, &self.engine, || {}).expect("the server opens it");
            assert_eq!(self.session.status(), &Status::Current);
        }

        fn accept(&mut self, shown: &Asked) {
            let name = self.held.accept(shown).expect("the prompt still asks this");
            self.open_file(&name);
        }

        /// `reuben-mcp`'s `open_instrument` of `path`.
        fn mcp_opens(&self, path: &str) -> SwapResult {
            self.mcp_opens_expecting(path, None)
        }

        fn mcp_opens_expecting(&self, path: &str, expect: Option<String>) -> SwapResult {
            open_instrument(
                &OpenInstrument {
                    name: path.to_string(),
                    expect,
                },
                self.link.structure(),
            )
            .expect("the open reached the engine")
            .output
        }

        /// What the engine names as playing.
        fn playing(&self) -> String {
            self.link
                .structure()
                .get_document()
                .expect("the engine answers")
                .source
                .expect("installed from a file")
        }

        fn spelled(&self, relative: &str) -> String {
            self.lib.join(relative).display().to_string()
        }
    }

    /// Compared by text alone, as the server names files: `..` folded, no file looked at, so a
    /// remote server's names compare the same as the editor's own.
    #[test]
    fn two_spellings_compare_by_text_alone() {
        let held = HeldSwaps::new(|| {});
        held.opened(Path::new("/no/such/dir/../pad.json"));
        assert!(!held.hold(Path::new("/no/such/./pad.json")));
        assert!(held.hold(Path::new("/no/such/dir/pad.json")));
    }

    #[test]
    fn an_open_of_the_open_file_installs_with_no_prompt() {
        within(HANG_SECS, || {
            let mut editor = Editor::open("held_same", "acid-techno.json");
            let opened = editor.mcp_opens(&editor.spelled("acid-techno.json"));
            assert!(opened.report.report.ok, "{:?}", opened.report.report);
            assert_eq!(opened.held, None);
            assert_eq!(editor.held.pending(), None);
            assert_eq!(editor.wakes.load(Ordering::SeqCst), 0);
            editor.engine.shutdown();
        });
    }

    #[test]
    fn another_spelling_of_the_open_file_is_the_open_file() {
        within(HANG_SECS, || {
            let mut editor = Editor::open("held_spelling", "acid-techno.json");
            let opened = editor.mcp_opens(&editor.spelled("voices/../acid-techno.json"));
            assert_eq!(opened.held, None);
            assert!(opened.report.report.ok, "{:?}", opened.report.report);
            editor.engine.shutdown();
        });
    }

    #[test]
    fn an_open_of_another_instrument_is_held_and_accepting_opens_it() {
        within(HANG_SECS, || {
            let mut editor = Editor::open("held_accept", "acid-techno.json");
            let acid = editor.spelled("acid-techno.json");
            let groovebox = editor.spelled("groovebox.json");

            let asked_at = Instant::now();
            let opened = editor.mcp_opens(&groovebox);
            assert!(
                asked_at.elapsed() < Duration::from_secs(1),
                "a held open answers at once, not after the user does"
            );
            let held = opened.held.expect("the open is held");
            assert!(!opened.report.report.ok);
            assert_eq!(held.requested, groovebox);
            assert_eq!(held.playing.as_deref(), Some(acid.as_str()));
            assert_eq!(editor.playing(), acid);
            assert_eq!(editor.wakes.load(Ordering::SeqCst), 1);
            let asked = editor.held.pending().expect("the user is asked");
            assert_eq!(asked.path, Path::new(&groovebox));

            editor.accept(&asked);
            assert_eq!(editor.held.pending(), None);
            assert_eq!(editor.session.top(), groovebox);
            assert_eq!(
                editor.playing(),
                groovebox,
                "the source the agent asked for"
            );

            // Open now, so the agent's next open of it is not held.
            assert_eq!(editor.mcp_opens(&groovebox).held, None);
            editor.engine.shutdown();
        });
    }

    #[test]
    fn declining_a_held_open_changes_nothing() {
        within(HANG_SECS, || {
            let mut editor = Editor::open("held_decline", "acid-techno.json");
            let acid = editor.spelled("acid-techno.json");
            assert!(editor
                .mcp_opens(&editor.spelled("groovebox.json"))
                .held
                .is_some());

            let asked = editor.held.pending().expect("the user is asked");
            editor.held.decline(&asked);
            assert_eq!(editor.held.pending(), None);
            assert_eq!(editor.session.top(), acid);
            assert_eq!(editor.playing(), acid);
            editor.engine.shutdown();
        });
    }

    #[test]
    fn a_voice_inside_the_open_instrument_is_held() {
        within(HANG_SECS, || {
            let mut editor = Editor::open("held_voice", "acid-techno.json");
            let acid = editor.spelled("acid-techno.json");
            let opened = editor.mcp_opens(&editor.spelled("voices/kick-voice.json"));
            assert!(opened.held.is_some(), "{:?}", opened.report);
            assert_eq!(editor.playing(), acid);
            editor.engine.shutdown();
        });
    }

    #[test]
    fn viewing_a_voice_leaves_the_top_level_file_the_open_one() {
        within(HANG_SECS, || {
            let mut editor = Editor::open("held_viewing", "acid-techno.json");
            let kick = editor.spelled("voices/kick-voice.json");
            editor.session.enter("/kick_v").expect("a voicer");
            assert_eq!(
                canonical(Path::new(&editor.session.viewed().source)),
                canonical(Path::new(&kick))
            );

            let acid = editor.spelled("acid-techno.json");
            let opened = editor.mcp_opens(&acid);
            assert_eq!(opened.held, None);
            assert!(opened.report.report.ok, "{:?}", opened.report.report);
            assert_eq!(editor.held.pending(), None);

            assert!(editor.mcp_opens(&kick).held.is_some(), "the viewed voice");
            assert_eq!(editor.playing(), acid);
            editor.engine.shutdown();
        });
    }

    #[test]
    fn another_file_spelled_through_dot_dot_is_still_held() {
        within(HANG_SECS, || {
            let mut editor = Editor::open("held_other_spelling", "acid-techno.json");
            let opened = editor.mcp_opens(&editor.spelled("voices/../groovebox.json"));
            assert!(opened.held.is_some(), "{:?}", opened.report);
            editor.engine.shutdown();
        });
    }

    #[test]
    fn a_second_held_open_replaces_the_first_and_an_answer_to_the_first_does_nothing() {
        within(HANG_SECS, || {
            let mut editor = Editor::open("held_twice", "acid-techno.json");
            assert!(editor
                .mcp_opens(&editor.spelled("groovebox.json"))
                .held
                .is_some());
            let first = editor.held.pending().expect("the user is asked");
            let default = editor.spelled("default.json");
            assert!(editor.mcp_opens(&default).held.is_some());
            assert_eq!(editor.wakes.load(Ordering::SeqCst), 2);
            let second = editor.held.pending().expect("the user is asked");
            assert_eq!(second.path, Path::new(&default));

            assert_eq!(editor.held.accept(&first), None);
            editor.held.decline(&first);
            assert_eq!(editor.held.pending(), Some(second));
            editor.engine.shutdown();
        });
    }

    #[test]
    fn a_stale_expect_conflicts_and_asks_nothing() {
        within(HANG_SECS, || {
            let mut editor = Editor::open("held_expect", "acid-techno.json");
            let answer = editor.mcp_opens_expecting(
                &editor.spelled("groovebox.json"),
                Some("0badc0de".to_string()),
            );
            assert!(answer.conflict.is_some(), "{answer:?}");
            assert_eq!(answer.held, None);
            assert_eq!(editor.held.pending(), None);
            editor.engine.shutdown();
        });
    }

    #[test]
    fn an_open_of_a_document_that_does_not_load_is_held_too() {
        within(HANG_SECS, || {
            let mut editor = Editor::open("held_broken", "acid-techno.json");
            let acid = editor.spelled("acid-techno.json");
            let broken = editor.spelled("broken.json");
            std::fs::write(
                &broken,
                r#"{"format_version":3,"instrument":"t","nodes":[{"#,
            )
            .expect("write a broken document");
            // Opening it would still unload what plays, so the user is asked.
            let opened = editor.mcp_opens(&broken);
            assert!(opened.held.is_some(), "{opened:?}");
            assert!(editor.held.pending().is_some());
            assert_eq!(editor.playing(), acid);
            editor.engine.shutdown();
        });
    }

    #[test]
    fn opening_the_file_asked_about_clears_the_prompt() {
        within(HANG_SECS, || {
            let mut editor = Editor::open("held_opened", "acid-techno.json");
            let groovebox = editor.spelled("groovebox.json");
            assert!(editor.mcp_opens(&groovebox).held.is_some());
            editor.open_file(&groovebox);
            assert_eq!(editor.held.pending(), None);
            assert_eq!(editor.playing(), groovebox);
            editor.engine.shutdown();
        });
    }
}

//! A Swap `reuben-mcp` asks the embedded engine for, of an instrument other than the one open
//! here: held rather than installed, so a background agent does not replace what the user is
//! working on, and put to the user to accept or decline.
//!
//! A Swap of the open top-level file is not held: it installs what the file watcher would install
//! anyway. Any other source is, a voice or subpatch file inside the open instrument's tree
//! included, since installing it would replace the top-level instrument.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use eframe::egui;
use reuben_native::structure::SwapHold;

/// The open instrument and the one Swap waiting on the user, shared between the engine's
/// structure server, which holds a Swap, and the UI thread, which asks about it.
#[derive(Clone)]
pub struct HeldSwaps {
    inner: Arc<Inner>,
}

struct Inner {
    /// The open top-level file, canonical. `None` while nothing is open, when nothing is held.
    open: Mutex<Option<PathBuf>>,
    /// The source the user is being asked about. A later held Swap replaces it.
    pending: Mutex<Option<PathBuf>>,
    wake: Box<dyn Fn() + Send + Sync>,
}

impl HeldSwaps {
    /// `wake` is called from the structure server's thread when a Swap is held, so the prompt is
    /// drawn without waiting for input.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(Inner {
                open: Mutex::new(None),
                pending: Mutex::new(None),
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
        let mut pending = lock(&self.inner.pending);
        if pending.as_ref() == Some(&top) {
            *pending = None;
        }
        *lock(&self.inner.open) = Some(top);
    }

    /// The source the user is being asked to open, if any.
    pub fn pending(&self) -> Option<PathBuf> {
        lock(&self.inner.pending).clone()
    }

    /// The user opened the source asked about; returns it to open.
    pub fn accept(&self) -> Option<PathBuf> {
        lock(&self.inner.pending).take()
    }

    /// The user kept what is open: nothing changes.
    pub fn decline(&self) {
        *lock(&self.inner.pending) = None;
    }

    /// Runs under the engine's Coordinator lock, so it records the question and returns; the
    /// user's answer comes later on the UI thread.
    fn hold(&self, path: &Path) -> bool {
        let asked = canonical(path);
        {
            let open = lock(&self.inner.open);
            match open.as_ref() {
                Some(open) if *open != asked => {}
                _ => return false,
            }
        }
        *lock(&self.inner.pending) = Some(asked);
        (self.inner.wake)();
        true
    }
}

/// `path` with links and `..` resolved, so two spellings of one file compare equal. A path that
/// does not resolve is compared made absolute against the working directory, which is where the
/// engine reads a Swap's path from.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// The two cells hold plain values that a panic cannot leave half-written.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Draw the prompt as a bar across the top of `ui` while a Swap is held. It takes no focus, so a
/// gesture under way on the canvas carries on until the user turns to it. Returns the source the
/// user chose to open.
pub fn show(ui: &mut egui::Ui, held: &HeldSwaps, open: Option<&Path>) -> Option<PathBuf> {
    let asked = held.pending()?;
    let mut answer = None;
    egui::Panel::top("held_swap").show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("reuben-mcp asked to play {}.", name(&asked)),
            )
            .on_hover_text(asked.display().to_string());
            if let Some(open) = open {
                ui.label(format!("{} keeps playing until you choose.", name(open)));
            }
            if ui.button(format!("Open {}", name(&asked))).clicked() {
                answer = held.accept();
            }
            if ui.button("Keep what is open").clicked() {
                held.decline();
            }
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

    use reuben_api::engine::{get_current_instrument, swap_instrument, SwapInstrument, SwapResult};
    use reuben_api::FsResolver;
    use reuben_mcp::EngineLink;
    use reuben_native::engine::{EngineConfig, RunningEngine, StartError};
    use reuben_native::test_support::{start_headless, within};

    use super::*;
    use crate::audio::tests::two_devices;
    use crate::audio::AudioSettings;
    use crate::config::test_support::TempConfig;
    use crate::engine::test_support::HANG_SECS;
    use crate::engine::Engine;
    use crate::session::test_support::Scratch;
    use crate::session::{Session, Status};
    use crate::start::{self, Launch};

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
        launch: Launch,
        audio: AudioSettings,
        engine: Engine,
        session: Session,
        link: EngineLink,
        _temp: TempConfig,
        _scratch: Scratch,
    }

    impl Editor {
        fn open(test: &str, instrument: &str) -> Self {
            let temp = TempConfig::new();
            let mut audio = AudioSettings::load_with(temp.dir(), two_devices);
            let scratch = Scratch::new(test);
            let lib = scratch.library();
            let wakes = Arc::new(AtomicUsize::new(0));
            let held = HeldSwaps::new({
                let wakes = Arc::clone(&wakes);
                move || {
                    wakes.fetch_add(1, Ordering::SeqCst);
                }
            });
            let launch = Launch {
                root: None,
                flags: Default::default(),
                swap_hold: Some(held.swap_hold()),
            };
            let mut engine = Engine::idle();
            let top = lib.join(instrument);
            let session = start::open(&top, &launch, &mut audio, &mut engine, || {}, headless);
            held.opened(&top);
            let structure = engine
                .running()
                .and_then(RunningEngine::structure_addr)
                .expect("the structure server bound");
            Self {
                lib,
                held,
                wakes,
                launch,
                audio,
                engine,
                session,
                link: EngineLink::new(structure.to_string()),
                _temp: temp,
                _scratch: scratch,
            }
        }

        /// `reuben-mcp`'s `swap_instrument` of `relative` in the library.
        fn mcp_swaps(&self, relative: &str) -> SwapResult {
            swap_instrument(
                &SwapInstrument {
                    path: self.lib.join(relative).display().to_string(),
                    expect: None,
                },
                self.link.structure(),
            )
            .expect("the swap reached the engine")
            .output
        }

        /// What `reuben-mcp`'s `get_current_instrument` names as playing.
        fn playing(&self) -> String {
            get_current_instrument(self.link.structure(), |source: Option<&str>| {
                FsResolver::for_document(source.expect("installed from a file")).stat_only()
            })
            .expect("the engine answers")
            .output
            .source
            .expect("installed from a file")
        }

        /// What the editor does with the source the user chose to open: open it as the start
        /// screen does.
        fn answer(&mut self, answer: Option<PathBuf>) {
            let Some(path) = answer else {
                return;
            };
            self.session = start::open(
                &path,
                &self.launch,
                &mut self.audio,
                &mut self.engine,
                || {},
                |_| panic!("a running engine is not started again"),
            );
            self.held.opened(&path);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(HANG_SECS);
            while self.session.status() != &Status::Current {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the Swap never landed"
                );
                self.session
                    .poll(std::time::Instant::now(), &|| self.engine.channel());
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }

        fn spelled(&self, relative: &str) -> String {
            self.lib.join(relative).display().to_string()
        }
    }

    #[test]
    fn a_swap_of_the_open_file_installs_with_no_prompt() {
        within(HANG_SECS, || {
            let mut editor = Editor::open("held_same", "acid-techno.json");
            let swapped = editor.mcp_swaps("acid-techno.json");
            assert!(swapped.report.report.ok, "{:?}", swapped.report.report);
            assert_eq!(swapped.held, None);
            assert_eq!(editor.held.pending(), None);
            assert_eq!(editor.wakes.load(Ordering::SeqCst), 0);
            editor.engine.shutdown();
        });
    }

    #[test]
    fn another_spelling_of_the_open_file_is_the_open_file() {
        within(HANG_SECS, || {
            let mut editor = Editor::open("held_spelling", "acid-techno.json");
            let swapped = editor.mcp_swaps("voices/../acid-techno.json");
            assert_eq!(swapped.held, None);
            assert!(swapped.report.report.ok, "{:?}", swapped.report.report);
            editor.engine.shutdown();
        });
    }

    #[test]
    fn a_swap_of_another_instrument_is_held_and_accepting_opens_it() {
        within(HANG_SECS, || {
            let mut editor = Editor::open("held_accept", "acid-techno.json");
            let acid = editor.spelled("acid-techno.json");
            let groovebox = editor.lib.join("groovebox.json");

            let asked_at = std::time::Instant::now();
            let swapped = editor.mcp_swaps("groovebox.json");
            assert!(
                asked_at.elapsed() < std::time::Duration::from_secs(1),
                "a held swap answers at once, not after the user does"
            );
            let held = swapped.held.expect("the swap is held");
            assert!(!swapped.report.report.ok);
            assert_eq!(held.playing.as_deref(), Some(acid.as_str()));
            assert_eq!(editor.playing(), acid);
            assert_eq!(editor.wakes.load(Ordering::SeqCst), 1);
            let asked = editor.held.pending().expect("the user is asked");
            assert_eq!(asked, canonical(&groovebox));

            let answer = editor.held.accept();
            editor.answer(answer);
            assert_eq!(editor.held.pending(), None);
            assert_eq!(canonical(editor.session.top()), canonical(&groovebox));
            assert_eq!(
                canonical(Path::new(&editor.playing())),
                canonical(&groovebox)
            );

            // Open now, so the agent's next swap of it is not held.
            let again = editor.mcp_swaps("groovebox.json");
            assert_eq!(again.held, None);
            editor.engine.shutdown();
        });
    }

    #[test]
    fn declining_a_held_swap_changes_nothing() {
        within(HANG_SECS, || {
            let mut editor = Editor::open("held_decline", "acid-techno.json");
            let acid = editor.spelled("acid-techno.json");
            assert!(editor.mcp_swaps("groovebox.json").held.is_some());

            editor.held.decline();
            assert_eq!(editor.held.pending(), None);
            assert_eq!(editor.session.top(), Path::new(&acid));
            assert_eq!(editor.playing(), acid);
            editor.engine.shutdown();
        });
    }

    #[test]
    fn a_voice_inside_the_open_instrument_is_held() {
        within(HANG_SECS, || {
            let mut editor = Editor::open("held_voice", "acid-techno.json");
            let acid = editor.spelled("acid-techno.json");
            let swapped = editor.mcp_swaps("voices/kick-voice.json");
            assert!(swapped.held.is_some(), "{:?}", swapped.report);
            assert_eq!(editor.playing(), acid);
            editor.engine.shutdown();
        });
    }

    #[test]
    fn a_second_held_swap_replaces_the_first() {
        within(HANG_SECS, || {
            let mut editor = Editor::open("held_twice", "acid-techno.json");
            assert!(editor.mcp_swaps("groovebox.json").held.is_some());
            assert!(editor.mcp_swaps("default.json").held.is_some());
            assert_eq!(
                editor.held.pending(),
                Some(canonical(&editor.lib.join("default.json")))
            );
            editor.engine.shutdown();
        });
    }

    #[test]
    fn a_stale_expect_conflicts_and_asks_nothing() {
        within(HANG_SECS, || {
            let mut editor = Editor::open("held_expect", "acid-techno.json");
            let answer = swap_instrument(
                &SwapInstrument {
                    path: editor.spelled("groovebox.json"),
                    expect: Some("0badc0de".to_string()),
                },
                editor.link.structure(),
            )
            .expect("the swap reached the engine")
            .output;
            assert!(answer.conflict.is_some(), "{answer:?}");
            assert_eq!(answer.held, None);
            assert_eq!(editor.held.pending(), None);
            editor.engine.shutdown();
        });
    }

    #[test]
    fn the_editors_own_swap_of_another_file_is_not_held() {
        within(HANG_SECS, || {
            let mut editor = Editor::open("held_own", "acid-techno.json");
            let groovebox = editor.lib.join("groovebox.json");
            editor.answer(Some(groovebox.clone()));
            assert_eq!(editor.held.pending(), None);
            assert_eq!(
                canonical(Path::new(&editor.playing())),
                canonical(&groovebox)
            );
            editor.engine.shutdown();
        });
    }
}

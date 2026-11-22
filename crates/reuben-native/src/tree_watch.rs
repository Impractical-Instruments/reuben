//! Follows writes from outside the engine — a text editor, a script — to the open instrument's
//! files, and has the engine [reload](ChannelState::reload) them.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use reuben_api::engine::{ChannelState, Subscription};

/// How long the tree's files must go untouched before they are read again. One write is several
/// events — a temp file, a rename, sometimes a truncate before the bytes — and a program saving
/// two files touches them a few milliseconds apart; reading on the first event would Swap a state
/// that is gone by the time the Swap lands.
const QUIET: Duration = Duration::from_millis(50);

/// The longest a touch waits to be read while later touches keep arriving, so files written
/// without a pause — a script sweeping a value — are still followed.
pub const LONGEST_WAIT: Duration = Duration::from_millis(500);

/// How often an idle watcher wakes to see whether what is open changed, which moves the files to
/// watch. Shutdown does not wait on it: a drop wakes the thread at once.
const IDLE_POLL: Duration = Duration::from_millis(100);

/// What wakes the watcher's thread.
enum Wake {
    /// A watched file was touched, or the watch lost events and anything may have changed.
    Touched,
    Stop,
}

/// The running watcher. Dropping it stops and joins its thread.
pub struct TreeWatch {
    wake: Sender<Wake>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for TreeWatch {
    fn drop(&mut self) {
        let _ = self.wake.send(Wake::Stop);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Watch the files of `state`'s open instrument and [reload](ChannelState::reload) them a
/// debounced moment after each burst of writes. The files watched follow
/// [`tree`](ChannelState::tree): it is read again after each reload and after each install the
/// state publishes, so a newly referenced file, or a newly opened instrument, is watched.
///
/// A platform that cannot watch files leaves the engine playing and not following; the reason is
/// printed once.
pub fn spawn(state: ChannelState) -> TreeWatch {
    let (wake, woken) = mpsc::channel();
    let touched = wake.clone();
    let files = Arc::new(Mutex::new(BTreeSet::<PathBuf>::new()));
    let watched = Arc::clone(&files);
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let ours = match event {
            // Reading a file is an event too on Linux, and every reload reads every file: a read
            // that counted as a touch would reload forever.
            Ok(event) if matches!(event.kind, EventKind::Access(_)) => false,
            // Events were lost or the watch broke: what changed is unknown, so look.
            Ok(event) if event.need_rescan() => true,
            Err(_) => true,
            Ok(event) => {
                let files = watched.lock().unwrap_or_else(|e| e.into_inner());
                // The file itself, or a directory above it that appeared, moved or went.
                (event.paths.iter()).any(|path| files.iter().any(|file| file.starts_with(path)))
            }
        };
        if ours {
            let _ = touched.send(Wake::Touched);
        }
    });
    // Aimed before this returns, so a write made once it has returned is seen.
    let watch = match watcher {
        Ok(watcher) => {
            let mut watch = Watch {
                watcher,
                files,
                directories: BTreeSet::new(),
            };
            let _ = watch.set(&state.tree());
            Some((watch, state.subscription()))
        }
        Err(e) => {
            eprintln!("not following edits to the instrument's files: {e}");
            None
        }
    };
    let handle = std::thread::Builder::new()
        .name("tree-watch".to_string())
        .spawn(move || {
            if let Some((mut watch, events)) = watch {
                follow(&state, &mut watch, &woken, &events);
            }
        })
        .expect("spawn tree-watch thread");
    TreeWatch {
        wake,
        handle: Some(handle),
    }
}

/// The watcher's loop: debounce touches into one reload per burst, and re-aim the watch whenever
/// what is open may have moved.
fn follow(state: &ChannelState, watch: &mut Watch, woken: &Receiver<Wake>, events: &Subscription) {
    let mut first: Option<Instant> = None;
    let mut due: Option<Instant> = None;
    loop {
        let wait = due.map_or(IDLE_POLL, |due| {
            due.saturating_duration_since(Instant::now()).min(IDLE_POLL)
        });
        match woken.recv_timeout(wait) {
            Ok(Wake::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Ok(Wake::Touched) => {
                let now = Instant::now();
                let first = *first.get_or_insert(now);
                due = Some((now + QUIET).min(first + LONGEST_WAIT));
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        let mut aim = false;
        if due.is_some_and(|due| Instant::now() >= due) {
            (first, due) = (None, None);
            if state.try_reload().is_err() {
                // An earlier swap is still in flight and nothing was read: the touch is not
                // spent, so read again after another quiet spell.
                due = Some(Instant::now() + QUIET);
            }
            aim = true;
        }
        // An install — this reload's, or one a client made — may have changed the tree.
        loop {
            match events.try_recv() {
                Ok(_) => aim = true,
                Err(TryRecvError::Empty) => break,
                // The state was closed: the engine is going down.
                Err(TryRecvError::Disconnected) => return,
            }
        }
        // A file newly in the tree may have been written between the install that read it and
        // the watch reaching it, which no touch reports: read the tree once more to see.
        if aim && watch.set(&state.tree()) && due.is_none() {
            first = Some(Instant::now());
            due = Some(Instant::now() + QUIET);
        }
    }
}

/// The directories watched for a set of files.
///
/// It watches each file's directory rather than the file. A document verb replaces a document by
/// renaming a temp file over it, which leaves a watch on the file itself attached to the file that
/// is gone; and a file that is not there yet has only a directory to watch. Where the directory is
/// not there either, the nearest one above it that is gets the watch, and the directory appearing
/// is the touch.
struct Watch {
    watcher: RecommendedWatcher,
    /// What the notify callback matches an event's paths against.
    files: Arc<Mutex<BTreeSet<PathBuf>>>,
    directories: BTreeSet<PathBuf>,
}

impl Watch {
    /// Watch exactly `sources`, the canonical sources of the open instrument. A source that is not
    /// an absolute path names no file this host can watch, and is skipped. Returns whether a file
    /// is watched now that was not before.
    fn set(&mut self, sources: &BTreeSet<String>) -> bool {
        let mut files = BTreeSet::new();
        let mut directories = BTreeSet::new();
        for path in sources.iter().map(Path::new).filter(|p| p.is_absolute()) {
            let Some(directory) = path.parent() else {
                continue;
            };
            files.insert(path.to_path_buf());
            // A backend that reports the path a symlinked directory resolves to has to find the
            // file under that spelling as well.
            if let (Ok(real), Some(name)) = (directory.canonicalize(), path.file_name()) {
                files.insert(real.join(name));
            }
            if let Some(there) = directory.ancestors().find(|above| above.is_dir()) {
                directories.insert(there.to_path_buf());
            }
        }
        let added = {
            let mut watched = self.files.lock().unwrap_or_else(|e| e.into_inner());
            let added = !files.is_subset(&watched);
            *watched = files;
            added
        };

        for gone in self.directories.difference(&directories) {
            let _ = self.watcher.unwatch(gone);
        }
        let mut watching = BTreeSet::new();
        for directory in directories {
            // Asked for again even when it was watched before: a directory removed and made again
            // is a new directory, and the watch on the old one went with it.
            match self.watcher.watch(&directory, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    watching.insert(directory);
                }
                Err(e) => eprintln!("not following edits in {}: {e}", directory.display()),
            }
        }
        self.directories = watching;
        added
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Receiver;

    use reuben_api::authoring::{DocumentHistory, Resources};
    use reuben_api::engine::{ChangedBy, ChannelState, Event, InProcess};
    use reuben_api::render::AudioConfig;
    use reuben_api::staged::Staged;
    use reuben_api::FsResolver;

    use super::*;
    use crate::control::ring_pair;
    use crate::diagnostics::Diagnostics;
    use crate::engine_channel::{HeadlessRenderConfig, NativeHost};
    use crate::test_support::FakeCallback;

    const TOP: &str = r#"{"format_version":3,"instrument":"top",
        "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
        "nodes":[{"type":"oscillator","address":"/osc"}]}"#;

    /// Nests `voices/v.json`, so the voice sits in a directory of its own.
    const NESTING: &str = r#"{"format_version":3,"instrument":"top",
        "resources":{"v":"voices/v.json"},
        "interface":{"outputs":{"out":{"from":"/voice.out"}}},
        "nodes":[{"type":"subpatch","address":"/voice","patch":"v"}]}"#;

    const VOICE: &str = r#"{"format_version":3,"instrument":"v",
        "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
        "nodes":[{"type":"oscillator","address":"/osc"}]}"#;

    /// The first [`Event::Changed`] within `limit`, skipping any other event.
    fn changed_within(events: &Receiver<Event>, limit: Duration) -> Option<(String, ChangedBy)> {
        let deadline = Instant::now() + limit;
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            match events.recv_timeout(left) {
                Ok(Event::Changed { hash, by }) => return Some((hash, by)),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    }

    /// The installed content hash. The event that announced an install is published while the
    /// Coordinator is still held, so this waits for it to be let go.
    fn installed_hash(state: &ChannelState) -> String {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match state.try_installed() {
                reuben_api::engine::Installed::Known { content_hash, .. } => return content_hash,
                reuben_api::engine::Installed::Busy if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                other => panic!("{other:?}"),
            }
        }
    }

    /// Serve `top` from a fresh directory holding `files`, watch it, save `edited` over the file
    /// at `edit` the way a text editor does, and expect the engine to follow within 2 s.
    fn follows(case: &str, top: &str, files: &[(&str, &str)], edit: &str, edited: &str) {
        // Process-unique: a fixed name collides between concurrent checkouts, and this test
        // deletes the directory it is about to use.
        let dir = std::env::temp_dir().join(format!("reuben_{case}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (name, text) in [("top.json", top)].iter().chain(files) {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().expect("a directory")).expect("case dir");
            std::fs::write(&path, text).expect("seed the open instrument");
        }
        let top_path = dir.join("top.json");

        let staged: Staged<Box<dyn Resources + Send + Sync>> =
            Staged::new(Box::new(FsResolver::for_instrument(&top_path)));
        let (coordinator, side, _warnings) = reuben_api::engine::install_initial(
            top,
            staged.clone(),
            AudioConfig::new(48_000.0, 128),
        )
        .expect("initial install");
        let (control_tx, control_rx) = ring_pair(Diagnostics::new());
        let host = NativeHost::new(Diagnostics::new(), control_tx).with_render_config(Arc::new(
            HeadlessRenderConfig {
                opened_input_channels: 0,
            },
        ));
        let state = ChannelState::new(
            coordinator,
            Arc::new(host),
            Arc::new(DocumentHistory::new(staged)),
        )
        .with_installed_source(Some(top_path.display().to_string()));
        let cb = FakeCallback::spawn(side, control_rx);
        let events = InProcess::new(state.clone()).subscribe();
        let watch = spawn(state.clone());

        // A text editor's save: a temp file renamed over the document.
        let target = dir.join(edit);
        let temp = target.with_extension("json.tmp");
        std::fs::write(&temp, edited).expect("write");
        std::fs::rename(&temp, &target).expect("rename over");

        let (hash, by) =
            changed_within(&events, Duration::from_secs(2)).expect("a change within 2 s");
        assert_eq!(by, ChangedBy::Outside);
        assert_eq!(installed_hash(&state), hash);

        drop(watch);
        state.close();
        cb.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An outside write that lands while the engine's own swap is still in flight is followed
    /// once the render side takes that swap home, with nothing else touching the files.
    #[test]
    fn an_outside_write_during_a_swap_in_flight_is_followed_once_it_lands() {
        let dir = std::env::temp_dir().join(format!(
            "reuben_tree_watch_in_flight_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("case dir");
        let top_path = dir.join("top.json");
        std::fs::write(&top_path, TOP).expect("seed the open instrument");

        let staged: Staged<Box<dyn Resources + Send + Sync>> =
            Staged::new(Box::new(FsResolver::for_instrument(&top_path)));
        let (coordinator, side, _warnings) = reuben_api::engine::install_initial(
            TOP,
            staged.clone(),
            AudioConfig::new(48_000.0, 128),
        )
        .expect("initial install");
        let (control_tx, control_rx) = ring_pair(Diagnostics::new());
        let host = NativeHost::new(Diagnostics::new(), control_tx).with_render_config(Arc::new(
            HeadlessRenderConfig {
                opened_input_channels: 0,
            },
        ));
        let top_s = top_path.display().to_string();
        let state = ChannelState::new(
            coordinator,
            Arc::new(host),
            Arc::new(DocumentHistory::new(staged)),
        )
        .with_installed_source(Some(top_s.clone()));
        let events = InProcess::new(state.clone()).subscribe();
        let watch = spawn(state.clone());

        // No render side yet: this edit's swap stays in flight.
        let edited = reuben_api::engine::dispatch_request(
            &state,
            reuben_api::engine::Request::Call {
                tool: "set_instrument_name".to_string(),
                args: serde_json::json!({"source": top_s, "name": "ours"}),
            },
        );
        assert!(
            matches!(edited, reuben_api::engine::Response::Called { .. }),
            "{edited:?}"
        );
        let temp = top_path.with_extension("json.tmp");
        std::fs::write(&temp, TOP.replace(r#""top""#, r#""theirs""#)).expect("write");
        std::fs::rename(&temp, &top_path).expect("rename over");
        // Long enough for the watcher to have read the files once and found the swap in flight.
        std::thread::sleep(QUIET * 6);

        let cb = FakeCallback::spawn(side, control_rx);
        let deadline = Instant::now() + Duration::from_secs(2);
        let followed = loop {
            match changed_within(&events, deadline.saturating_duration_since(Instant::now())) {
                Some((hash, ChangedBy::Outside)) => break Some(hash),
                Some(_) => continue,
                None => break None,
            }
        };
        let followed = followed.expect("the outside write followed within 2 s");
        assert_eq!(installed_hash(&state), followed);

        drop(watch);
        state.close();
        cb.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An instrument opened a moment ago is not watched until the watcher hears of the open; a
    /// write to it in that moment is followed all the same.
    #[test]
    fn a_write_right_after_an_open_is_followed() {
        let dir = std::env::temp_dir().join(format!(
            "reuben_tree_watch_after_open_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("case dir");
        let top_path = dir.join("top.json");
        std::fs::write(&top_path, TOP).expect("seed the open instrument");
        let other = dir.join("other.json");
        std::fs::write(&other, TOP).expect("seed the instrument to open");

        let staged: Staged<Box<dyn Resources + Send + Sync>> =
            Staged::new(Box::new(FsResolver::for_instrument(&top_path)));
        let (coordinator, side, _warnings) = reuben_api::engine::install_initial(
            TOP,
            staged.clone(),
            AudioConfig::new(48_000.0, 128),
        )
        .expect("initial install");
        let (control_tx, control_rx) = ring_pair(Diagnostics::new());
        let host = NativeHost::new(Diagnostics::new(), control_tx).with_render_config(Arc::new(
            HeadlessRenderConfig {
                opened_input_channels: 0,
            },
        ));
        let state = ChannelState::new(
            coordinator,
            Arc::new(host),
            Arc::new(DocumentHistory::new(staged)),
        )
        .with_installed_source(Some(top_path.display().to_string()));
        let cb = FakeCallback::spawn(side, control_rx);
        let events = InProcess::new(state.clone()).subscribe();
        let watch = spawn(state.clone());

        let opened = reuben_api::engine::dispatch_request(
            &state,
            reuben_api::engine::Request::Open {
                name: other.display().to_string(),
                expect: None,
            },
        );
        assert!(
            matches!(&opened, reuben_api::engine::Response::SwapReport(r) if r.report.ok),
            "{opened:?}"
        );
        let temp = other.with_extension("json.tmp");
        std::fs::write(&temp, TOP.replace(r#""top""#, r#""theirs""#)).expect("write");
        std::fs::rename(&temp, &other).expect("rename over");

        let deadline = Instant::now() + Duration::from_secs(2);
        let followed = loop {
            match changed_within(&events, deadline.saturating_duration_since(Instant::now())) {
                Some((hash, ChangedBy::Outside)) => break Some(hash),
                Some(_) => continue,
                None => break None,
            }
        };
        let followed = followed.expect("the write followed within 2 s");
        assert_eq!(installed_hash(&state), followed);

        drop(watch);
        state.close();
        cb.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn watcher_follows_a_file_edit() {
        let renamed = TOP.replace(r#""top""#, r#""renamed""#);
        follows("tree_watch_top", TOP, &[], "top.json", &renamed);
    }

    #[test]
    fn watcher_follows_an_edit_to_a_nested_file() {
        let moved = VOICE.replace("/osc", "/osc2");
        follows(
            "tree_watch_nested",
            NESTING,
            &[("voices/v.json", VOICE)],
            "voices/v.json",
            &moved,
        );
    }
}

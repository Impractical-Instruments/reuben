//! The open instrument as it is on disk: its [`Tree`] of files, the watcher over them, and the
//! Swap that follows every change to any of them.
//!
//! Nothing here holds a document that differs from its file. A change — the editor's own write,
//! `reuben-mcp`'s, a text editor's — is seen by the watcher, the whole tree is read again, and the
//! top-level instrument is swapped in. A tree that does not load is shown and not swapped: the
//! engine keeps playing the last one that did, until the files load again.

mod tree;
mod undo;
mod view;
mod watch;

use std::collections::{BTreeMap, BTreeSet};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reuben_api::authoring::DocumentHistory;
use reuben_api::engine::{swap_instrument, Channel, SwapInstrument, SwapResult};
use reuben_api::FsResolver;

pub use self::tree::{Document, Tree};
pub use self::undo::{Direction, Step, Undo};
pub use self::view::{shortcut, show};
use self::watch::Watch;

/// How long the tree's files must go untouched before they are read again. One write is several
/// events — a temp file, a rename, sometimes a truncate before the bytes — and a program saving
/// two files touches them a few milliseconds apart; reading on the first event would Swap a state
/// that is gone by the time the Swap lands.
const QUIET: Duration = Duration::from_millis(50);

/// The longest a touch waits to be read while later touches keep arriving, so files written
/// without a pause — a script sweeping a value — are still followed.
const LONGEST_WAIT: Duration = Duration::from_millis(500);

/// Whether the engine plays what is on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Current,
    /// No engine is running, so nothing plays.
    NoEngine,
    /// A Swap of what is on disk is running.
    Swapping,
    /// It does not, and why: the files do not load, or the engine refused the Swap. The engine
    /// keeps playing what it had.
    Held(Vec<String>),
}

/// What one [`Session::poll`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Poll {
    /// The tree was read since the last poll, by it or by [`Session::refresh`], and differed
    /// from the one before.
    pub reloaded: bool,
    /// Call again no later than this, a touch being inside its quiet period.
    pub wait: Option<Duration>,
}

type Wake = Arc<dyn Fn() + Send + Sync>;

pub struct Session {
    top: PathBuf,
    root: Option<PathBuf>,
    tree: Tree,
    /// `Err` when the platform gave no watcher: the tree is then read only after the editor's own
    /// writes.
    watch: Result<Watch, String>,
    /// The paths of the nested documents that read in the last tree that loaded. One of these
    /// going missing holds the Swap, where a reference that never resolved does not.
    loaded: BTreeSet<PathBuf>,
    /// When the touched files are due to be read.
    due: Option<Instant>,
    /// When the first touch not yet read was seen.
    touched_at: Option<Instant>,
    /// The tree changed since [`poll`](Self::poll) last said so.
    changed: bool,
    status: Status,
    swapping: bool,
    /// The tree changed while a Swap was running, so what that Swap installs is already stale.
    swap_again: bool,
    results: (Sender<SwapOutcome>, Receiver<SwapOutcome>),
    wake: Wake,
    undo: Undo,
    /// The store each document is written through, kept for the session's life so the states a
    /// step names stay held for a restore.
    histories: BTreeMap<String, DocumentHistory<FsResolver>>,
    #[cfg(test)]
    touches: usize,
}

type SwapOutcome = Result<SwapResult, String>;

impl Session {
    /// Open the instrument at `top`, which the engine is taken to have started on. `root` is the
    /// library root a reference falls back to, as for the engine.
    ///
    /// `wake` is called from another thread whenever [`poll`](Self::poll) has something to do.
    pub fn open(top: &Path, root: Option<&Path>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let wake: Wake = Arc::new(wake);
        let tree = Tree::resolve(top, root);
        let mut watch = Watch::new({
            let wake = Arc::clone(&wake);
            move || wake()
        });
        if let Ok(watch) = &mut watch {
            watch.set(tree.documents().iter().map(|d| d.path.as_path()));
        }
        let blocked = tree.blocked();
        Self {
            top: top.to_path_buf(),
            root: root.map(Path::to_path_buf),
            loaded: if blocked.is_empty() {
                read_paths(&tree)
            } else {
                BTreeSet::new()
            },
            tree,
            watch,
            due: None,
            touched_at: None,
            changed: false,
            status: if blocked.is_empty() {
                Status::Current
            } else {
                Status::Held(blocked)
            },
            swapping: false,
            swap_again: false,
            results: mpsc::channel(),
            wake,
            undo: Undo::default(),
            histories: BTreeMap::new(),
            #[cfg(test)]
            touches: 0,
        }
    }

    pub fn top(&self) -> &Path {
        &self.top
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// Why the files, or some of them, are not being watched, when they are not.
    pub fn watch_error(&self) -> Option<String> {
        match &self.watch {
            Err(e) => Some(e.clone()),
            Ok(watch) if watch.unwatched().is_empty() => None,
            Ok(watch) => Some(watch.unwatched().join("; ")),
        }
    }

    /// The store to hand a `reuben-api` verb for the document `source`: the one the tree reads it
    /// through, wrapped in the history a restore returns it to an earlier state from. The same one
    /// for every call on `source`, for as long as the session is open, so the states its writes
    /// left stay held.
    pub fn history(&mut self, source: &str) -> &DocumentHistory<FsResolver> {
        let root = self.root.as_deref();
        self.histories
            .entry(source.to_string())
            .or_insert_with(|| DocumentHistory::new(tree::store(source, root)))
    }

    pub fn undo(&self) -> &Undo {
        &self.undo
    }

    pub fn undo_mut(&mut self) -> &mut Undo {
        &mut self.undo
    }

    /// Take in what happened since the last call: a finished Swap, and files touched long enough
    /// ago to read. `channel` is asked for the running engine's channel only when a Swap starts.
    pub fn poll(&mut self, now: Instant, channel: &dyn Fn() -> Option<Channel>) -> Poll {
        if let Ok(outcome) = self.results.1.try_recv() {
            self.swapped(outcome, channel);
        }
        if self.watch.as_ref().is_ok_and(Watch::touched) {
            let first = *self.touched_at.get_or_insert(now);
            self.due = Some((now + QUIET).min(first + LONGEST_WAIT));
        }
        let wait = match self.due {
            Some(due) if now >= due => {
                self.due = None;
                self.touched_at = None;
                #[cfg(test)]
                {
                    self.touches += 1;
                }
                self.refresh(channel);
                None
            }
            due => due.map(|due| due - now),
        };
        Poll {
            reloaded: std::mem::take(&mut self.changed),
            wait,
        }
    }

    /// Offer what is on disk to the engine again, as after a start that the files have moved on
    /// from.
    pub fn reinstall(&mut self, channel: &dyn Fn() -> Option<Channel>) {
        self.install(channel);
    }

    /// Read the tree from disk now and, when it differs from the one held, Swap it in. Returns
    /// whether it differed.
    ///
    /// Reading the same files twice Swaps once: this is what keeps the editor's own write, which
    /// calls this and then arrives again through the watcher, from being two Swaps.
    pub fn refresh(&mut self, channel: &dyn Fn() -> Option<Channel>) -> bool {
        let tree = Tree::resolve(&self.top, self.root.as_deref());
        // Even for an unchanged tree: a directory that came back holds no file yet, so the tree
        // reads as it did, and only a fresh watch sees the file arrive.
        if let Ok(watch) = &mut self.watch {
            watch.set(tree.documents().iter().map(|d| d.path.as_path()));
        }
        if tree == self.tree {
            return false;
        }
        self.tree = tree;
        self.changed = true;
        self.install(channel);
        true
    }

    /// Why the tree must not be swapped in; empty when it may be.
    fn blocked(&self) -> Vec<String> {
        let mut why = self.tree.blocked();
        for document in &self.tree.documents()[1..] {
            if document.read.is_err()
                && self.loaded.contains(&document.path)
                && !document.path.exists()
            {
                why.push(format!(
                    "{}: was part of the instrument and is gone",
                    document.path.display()
                ));
            }
        }
        why
    }

    fn install(&mut self, channel: &dyn Fn() -> Option<Channel>) {
        let blocked = self.blocked();
        if !blocked.is_empty() {
            self.status = Status::Held(blocked);
            self.swap_again = false;
            return;
        }
        self.loaded = read_paths(&self.tree);
        if self.swapping {
            self.status = Status::Swapping;
            self.swap_again = true;
            return;
        }
        let Some(channel) = channel() else {
            self.status = Status::NoEngine;
            return;
        };
        let args = SwapInstrument {
            path: self.top.display().to_string(),
            expect: None,
        };
        let done = self.results.0.clone();
        let wake = Arc::clone(&self.wake);
        // Off the UI thread: a Swap builds the whole new Engine before it returns.
        std::thread::spawn(move || {
            // A panic that took this thread down unreported would leave the session waiting on
            // this Swap for good.
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
                swap_instrument(&args, &channel)
                    .map(|answer| answer.output)
                    .map_err(|refusal| refusal.message)
            }))
            .unwrap_or_else(|_| Err("the Swap panicked".to_string()));
            if done.send(outcome).is_ok() {
                wake();
            }
        });
        self.swapping = true;
        self.status = Status::Swapping;
    }

    fn swapped(&mut self, outcome: SwapOutcome, channel: &dyn Fn() -> Option<Channel>) {
        self.swapping = false;
        if self.swap_again {
            self.swap_again = false;
            self.install(channel);
            return;
        }
        // Anything else means the tree stopped loading while this Swap ran, and that is the news.
        if self.status != Status::Swapping {
            return;
        }
        self.status = match outcome {
            Ok(result) if result.report.report.ok => Status::Current,
            Ok(result) => Status::Held(match result.conflict {
                Some(conflict) => vec![format!(
                    "the engine is playing {}, not the expected {}",
                    conflict.actual, conflict.expected
                )],
                None => result
                    .report
                    .report
                    .errors
                    .into_iter()
                    .map(|e| match e.node {
                        Some(node) => format!("{node}: {}", e.message),
                        None => e.message,
                    })
                    .collect(),
            }),
            Err(e) => Status::Held(vec![e]),
        };
    }
}

/// The paths of the nested documents of `tree` that read.
fn read_paths(tree: &Tree) -> BTreeSet<PathBuf> {
    (tree.documents()[1..].iter())
        .filter(|document| document.read.is_ok())
        .map(|document| document.path.clone())
        .collect()
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc::{self};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use reuben_api::engine::{
        get_current_instrument, install_initial, Channel, InProcess, Request, StructureState,
        Transport,
    };
    use reuben_api::render::AudioConfig;
    use reuben_api::FsResolver;
    use reuben_native::diagnostics::Diagnostics;
    use reuben_native::osc::ControlBatch;
    use reuben_native::structure::NativeHost;
    use reuben_native::test_support::FakeCallback;

    use super::{Session, Status};

    /// A scratch directory removed on drop, so a failed assertion leaves nothing behind.
    pub struct Scratch(PathBuf);

    impl Scratch {
        pub fn new(test: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "reuben_editor_session_{test}_{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create the scratch directory");
            Scratch(dir)
        }

        pub fn path(&self, relative: &str) -> PathBuf {
            self.0.join(relative)
        }

        /// Copy the whole instrument library to `lib/` and return that directory.
        pub fn library(&self) -> PathBuf {
            let lib = self.path("lib");
            copy_dir(
                &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../instruments"),
                &lib,
            );
            lib
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).expect("create the copy's directory");
        for entry in std::fs::read_dir(from).expect("list a library directory") {
            let entry = entry.expect("read a directory entry");
            let target = to.join(entry.file_name());
            if entry.file_type().expect("stat an entry").is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), &target).expect("copy a library file");
            }
        }
    }

    /// What a test makes the next Swap requests do on their way to the engine.
    #[derive(Debug, Default)]
    pub struct Fault {
        /// How long each Swap request is held before it reaches the engine, in milliseconds.
        pub delay_ms: AtomicUsize,
        /// The next Swap request panics instead of reaching the engine.
        pub panic_once: AtomicBool,
    }

    /// The engine's own transport, counting the Swap requests that cross it.
    #[derive(Debug)]
    struct Counting {
        inner: InProcess,
        swaps: Arc<AtomicUsize>,
        fault: Arc<Fault>,
    }

    impl Transport for Counting {
        fn round_trip(&self, line: &str, read_timeout: Duration) -> std::io::Result<String> {
            if matches!(Request::from_ndjson(line), Ok(Request::Swap { .. })) {
                self.swaps.fetch_add(1, Ordering::SeqCst);
                if self.fault.panic_once.swap(false, Ordering::SeqCst) {
                    panic!("a test asked this Swap to panic");
                }
                let delay = self.fault.delay_ms.load(Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(delay as u64));
            }
            self.inner.round_trip(line, read_timeout)
        }

        fn endpoint(&self) -> &str {
            self.inner.endpoint()
        }
    }

    /// A session on a scratch copy of a library instrument, and an engine playing that copy with
    /// no audio device: the Coordinator, host and window are the ones `reuben play` wires up, and a
    /// [`FakeCallback`] renders in place of the device.
    pub struct Rig {
        pub lib: PathBuf,
        pub top: PathBuf,
        pub session: Session,
        engine: InProcess,
        swaps: Arc<AtomicUsize>,
        pub fault: Arc<Fault>,
        _callback: FakeCallback,
        _scratch: Scratch,
    }

    /// How long a change may take to reach the engine before a test calls it lost.
    pub const SEEN: Duration = Duration::from_secs(10);
    /// How long a test keeps polling for a Swap that must not come. Many times the session's quiet
    /// period plus the watcher's latency.
    pub const SETTLED: Duration = Duration::from_millis(600);

    impl Rig {
        pub fn open(test: &str, instrument: &str) -> Self {
            let scratch = Scratch::new(test);
            let lib = scratch.library();
            let top = lib.join(instrument);
            let text = std::fs::read_to_string(&top).expect("read the fixture");
            let (coordinator, side, _warnings) = install_initial(
                &text,
                FsResolver::for_instrument(&top),
                AudioConfig::new(48_000.0, 128),
            )
            .expect("install the fixture");
            let (control, control_rx) = mpsc::channel::<ControlBatch>();
            let host = NativeHost::new(Diagnostics::new(), control);
            let state = StructureState::new(coordinator, Arc::new(host))
                .with_installed_source(Some(top.display().to_string()));
            Rig {
                session: Session::open(&top, None, || {}),
                lib,
                top,
                engine: InProcess::new(state),
                swaps: Arc::new(AtomicUsize::new(0)),
                fault: Arc::new(Fault::default()),
                _callback: FakeCallback::spawn(side, control_rx),
                _scratch: scratch,
            }
        }

        pub fn channel(&self) -> Channel {
            Channel::new(Counting {
                inner: self.engine.clone(),
                swaps: Arc::clone(&self.swaps),
                fault: Arc::clone(&self.fault),
            })
        }

        /// How many Swap requests have reached the engine.
        pub fn swaps(&self) -> usize {
            self.swaps.load(Ordering::SeqCst)
        }

        /// The content hash of what the engine is playing.
        pub fn playing(&self) -> String {
            get_current_instrument(&self.channel(), |source: Option<&str>| {
                FsResolver::for_document(source.expect("installed from a file")).stat_only()
            })
            .expect("the engine answers")
            .output
            .content_hash
        }

        /// What the session asks for the engine's channel.
        pub fn channel_source(&self) -> impl Fn() -> Option<Channel> {
            let engine = self.engine.clone();
            let swaps = Arc::clone(&self.swaps);
            let fault = Arc::clone(&self.fault);
            move || {
                Some(Channel::new(Counting {
                    inner: engine.clone(),
                    swaps: Arc::clone(&swaps),
                    fault: Arc::clone(&fault),
                }))
            }
        }

        pub fn poll(&mut self) -> bool {
            let channel = self.channel_source();
            self.session.poll(Instant::now(), &channel).reloaded
        }

        /// Poll until `done`, failing after [`SEEN`].
        pub fn poll_until(&mut self, what: &str, done: impl Fn(&Rig) -> bool) {
            let deadline = Instant::now() + SEEN;
            while !done(self) {
                assert!(Instant::now() < deadline, "never happened: {what}");
                self.poll();
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        /// Poll for [`SETTLED`], long enough for anything still on its way to arrive.
        pub fn settle(&mut self) {
            let until = Instant::now() + SETTLED;
            while Instant::now() < until {
                self.poll();
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                self.session.due.is_none() && !self.session.swapping,
                "still busy"
            );
        }

        /// How many times touched files came due and were read.
        pub fn touches(&self) -> usize {
            self.session.touches
        }

        pub fn status(&self) -> &Status {
            self.session.status()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use reuben_api::authoring::{
        set_instrument_constant, set_instrument_input, SetInstrumentConstant, SetInstrumentInput,
    };
    use reuben_api::FsResolver;
    use reuben_native::test_support::within;
    use serde_json::json;

    use std::sync::atomic::Ordering;

    use super::test_support::Rig;
    use super::*;

    const HANG_SECS: u64 = 60;

    fn spelled(path: &Path) -> String {
        path.display().to_string()
    }

    /// The write `reuben-mcp` makes for `set_instrument_input`: the same verb, through a store of
    /// its own scoped to the document. Returns the hash of what it wrote.
    fn sidecar_sets_cutoff(voice: &Path, cutoff: f64) -> String {
        let source = spelled(voice);
        let edit = set_instrument_input(
            &SetInstrumentInput {
                source: source.clone(),
                address: "/filter".to_string(),
                input: "cutoff".to_string(),
                value: json!(cutoff),
                expect: None,
            },
            &FsResolver::for_document(&source).stat_only(),
        )
        .expect("the verb ran")
        .output;
        assert!(edit.written, "{:?}", edit.report);
        edit.hash
    }

    fn voice_of(rig: &Rig) -> std::path::PathBuf {
        rig.lib.join("voices").join("default-voice.json")
    }

    fn hash_of(rig: &Rig, path: &Path) -> Option<String> {
        rig.session
            .tree()
            .documents()
            .iter()
            .find(|d| d.path == path)
            .and_then(|d| d.hash().map(str::to_string))
    }

    #[test]
    fn a_sidecar_write_to_a_voice_file_swaps_exactly_once() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("voice_write", "default.json");
            let voice = voice_of(&rig);
            let playing = rig.playing();

            let written = sidecar_sets_cutoff(&voice, 1200.0);
            rig.poll_until("the Swap", |rig| rig.swaps() == 1);
            rig.settle();

            assert_eq!(rig.swaps(), 1);
            assert_eq!(rig.status(), &Status::Current);
            assert_eq!(hash_of(&rig, &voice), Some(written), "the view is the file");
            assert_eq!(
                rig.playing(),
                playing,
                "the top-level document is the same one; only its voice moved"
            );
        });
    }

    #[test]
    fn a_sidecar_write_to_the_top_level_file_swaps_once_and_the_engine_plays_it() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("top_write", "default.json");
            let before = rig.playing();
            let source = spelled(&rig.top);

            let edit = set_instrument_constant(
                &SetInstrumentConstant {
                    source: source.clone(),
                    address: "/voicer".to_string(),
                    name: "voices".to_string(),
                    value: json!(4),
                    expect: None,
                },
                &FsResolver::for_document(&source).stat_only(),
            )
            .expect("the verb ran")
            .output;
            assert!(edit.written, "{:?}", edit.report);
            rig.poll_until("the Swap", |rig| rig.swaps() == 1);
            rig.settle();

            assert_eq!(rig.swaps(), 1);
            assert_eq!(rig.status(), &Status::Current);
            assert_ne!(edit.hash, before);
            assert_eq!(rig.playing(), edit.hash);
            assert_eq!(rig.session.tree().top().hash(), Some(edit.hash.as_str()));
        });
    }

    #[test]
    fn an_invalid_write_leaves_the_previous_instrument_installed_until_the_file_loads_again() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("invalid_write", "default.json");
            let voice = voice_of(&rig);
            let playing = rig.playing();
            let good = std::fs::read_to_string(&voice).unwrap();

            for (file, what) in [(&voice, "the voice"), (&rig.top.clone(), "the instrument")] {
                let was = std::fs::read_to_string(file).unwrap();
                let before = rig.swaps();
                std::fs::write(file, r#"{"format_version":3,"instrument":"t","nodes":[{"#).unwrap();
                rig.poll_until(what, |rig| matches!(rig.status(), Status::Held(_)));
                rig.settle();
                assert_eq!(
                    rig.swaps(),
                    before,
                    "{what}: nothing was offered to the engine"
                );
                assert_eq!(rig.playing(), playing, "{what}");
                let Status::Held(why) = rig.status() else {
                    unreachable!()
                };
                assert_eq!(why.len(), 1, "{why:?}");

                // Back as it was, which is not the tree last read: it loads, so it is swapped in.
                let swaps = rig.swaps();
                std::fs::write(file, was).unwrap();
                rig.poll_until("the Swap once it loads", |rig| rig.swaps() == swaps + 1);
                rig.settle();
                assert_eq!(rig.status(), &Status::Current, "{what}");
                assert_eq!(rig.swaps(), swaps + 1, "{what}");
            }
            assert_eq!(std::fs::read_to_string(&voice).unwrap(), good);
            assert_eq!(rig.playing(), playing);
        });
    }

    #[test]
    fn a_document_that_reads_but_does_not_load_is_held_with_its_node_named() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("unloadable", "default.json");
            let playing = rig.playing();
            let text = std::fs::read_to_string(&rig.top)
                .unwrap()
                .replace("/voicer.audio", "/nothing.audio");
            std::fs::write(&rig.top, text).unwrap();

            rig.poll_until("held", |rig| matches!(rig.status(), Status::Held(_)));
            rig.settle();
            assert_eq!(rig.swaps(), 0);
            assert_eq!(rig.playing(), playing);
            let Status::Held(why) = rig.status() else {
                unreachable!()
            };
            assert!(why.iter().any(|w| w.contains("/out")), "{why:?}");
        });
    }

    #[test]
    fn the_tree_follows_a_change_to_resources() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("resources", "default.json");
            let old = voice_of(&rig);
            let new = rig.lib.join("elsewhere").join("moved-voice.json");
            std::fs::create_dir_all(new.parent().unwrap()).unwrap();
            std::fs::copy(&old, &new).unwrap();

            let text = std::fs::read_to_string(&rig.top)
                .unwrap()
                .replace("voices/default-voice.json", "elsewhere/moved-voice.json");
            std::fs::write(&rig.top, text).unwrap();
            rig.poll_until("the Swap", |rig| rig.swaps() == 1);
            rig.settle();
            let files: Vec<&Path> = rig
                .session
                .tree()
                .documents()
                .iter()
                .map(|d| d.path.as_path())
                .collect();
            assert_eq!(files, [rig.top.as_path(), new.as_path()]);

            // The file that left the tree is no longer the instrument's, and the one that joined,
            // in a directory nothing watched before, is.
            sidecar_sets_cutoff(&old, 900.0);
            rig.settle();
            assert_eq!(rig.swaps(), 1);
            sidecar_sets_cutoff(&new, 900.0);
            rig.poll_until("the Swap", |rig| rig.swaps() == 2);
            rig.settle();
            assert_eq!(rig.swaps(), 2);
        });
    }

    #[test]
    fn two_writes_inside_the_quiet_period_are_one_swap_of_the_last() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("burst", "default.json");
            let voice = voice_of(&rig);
            sidecar_sets_cutoff(&voice, 1200.0);
            let last = sidecar_sets_cutoff(&voice, 800.0);
            rig.poll_until("the Swap", |rig| rig.swaps() == 1);
            rig.settle();
            assert_eq!(rig.swaps(), 1);
            assert_eq!(hash_of(&rig, &voice), Some(last));
        });
    }

    #[test]
    fn nothing_is_read_or_swapped_while_nothing_changes() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("idle", "acid-techno.json");
            // A refresh reads every file of the tree; the watcher must not take that for a change.
            assert!(!rig.session.refresh(&|| None));
            rig.settle();
            assert_eq!(rig.touches(), 0);
            assert_eq!(rig.swaps(), 0);
            assert_eq!(rig.status(), &Status::Current);
        });
    }

    #[test]
    fn with_no_engine_running_the_tree_still_follows_the_files() {
        let scratch = test_support::Scratch::new("no_engine");
        let lib = scratch.library();
        let voice = lib.join("voices").join("default-voice.json");
        let mut session = Session::open(&lib.join("default.json"), None, || {});
        let written = sidecar_sets_cutoff(&voice, 1200.0);
        assert!(session.refresh(&|| None));
        assert_eq!(session.status(), &Status::NoEngine);
        assert_eq!(session.tree().documents()[1].hash(), Some(written.as_str()));
    }

    #[test]
    fn a_voice_file_that_goes_missing_is_held_until_it_is_back() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("voice_gone", "default.json");
            let voice = voice_of(&rig);
            let playing = rig.playing();
            let kept = rig.lib.join("kept.json");
            std::fs::rename(&voice, &kept).unwrap();

            rig.poll_until("held", |rig| matches!(rig.status(), Status::Held(_)));
            rig.settle();
            assert_eq!(
                rig.swaps(),
                0,
                "an instrument with no voice was not offered"
            );
            assert_eq!(rig.playing(), playing);

            std::fs::rename(&kept, &voice).unwrap();
            rig.poll_until("the Swap once it is back", |rig| rig.swaps() == 1);
            rig.settle();
            assert_eq!(rig.status(), &Status::Current);
        });
    }

    #[test]
    fn a_directory_removed_and_made_again_is_followed_afterwards() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("dir_gone", "default.json");
            let voice = voice_of(&rig);
            let voices = voice.parent().unwrap().to_path_buf();
            let kept = rig.lib.join("kept-voices");
            std::fs::rename(&voices, &kept).unwrap();
            rig.poll_until("held", |rig| matches!(rig.status(), Status::Held(_)));
            rig.settle();
            assert_eq!(rig.swaps(), 0);

            // A new directory with the same files, as a checkout or a sync leaves.
            std::fs::create_dir(&voices).unwrap();
            for entry in std::fs::read_dir(&kept).unwrap() {
                let entry = entry.unwrap();
                std::fs::copy(entry.path(), voices.join(entry.file_name())).unwrap();
            }
            rig.poll_until("the Swap once it is back", |rig| rig.swaps() == 1);
            rig.settle();
            assert_eq!(rig.status(), &Status::Current);

            sidecar_sets_cutoff(&voice, 900.0);
            rig.poll_until("a write in the new directory", |rig| rig.swaps() == 2);
        });
    }

    #[test]
    fn a_change_while_a_swap_is_running_is_swapped_once_more_when_it_lands() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("mid_swap", "default.json");
            let voice = voice_of(&rig);
            rig.fault.delay_ms.store(300, Ordering::SeqCst);

            sidecar_sets_cutoff(&voice, 1200.0);
            rig.poll_until("the first Swap leaving", |rig| rig.swaps() == 1);
            let last = sidecar_sets_cutoff(&voice, 800.0);
            rig.poll_until("the second Swap", |rig| rig.swaps() == 2);
            rig.poll_until("it landing", |rig| rig.status() == &Status::Current);
            rig.settle();

            assert_eq!(rig.swaps(), 2);
            assert_eq!(hash_of(&rig, &voice), Some(last));
        });
    }

    #[test]
    fn a_swap_that_panics_is_reported_and_the_next_change_still_swaps() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("swap_panic", "default.json");
            let voice = voice_of(&rig);
            rig.fault.panic_once.store(true, Ordering::SeqCst);

            sidecar_sets_cutoff(&voice, 1200.0);
            rig.poll_until("the panic reported", |rig| {
                matches!(rig.status(), Status::Held(_))
            });
            rig.settle();

            sidecar_sets_cutoff(&voice, 800.0);
            rig.poll_until("the next Swap", |rig| rig.swaps() == 2);
            rig.poll_until("it landing", |rig| rig.status() == &Status::Current);
        });
    }

    #[test]
    fn a_swap_the_engine_cannot_be_reached_for_is_held() {
        let scratch = test_support::Scratch::new("unreachable");
        let lib = scratch.library();
        let voice = lib.join("voices").join("default-voice.json");
        let mut session = Session::open(&lib.join("default.json"), None, || {});
        #[derive(Debug)]
        struct Unreachable;
        impl reuben_api::engine::Transport for Unreachable {
            fn round_trip(&self, _: &str, _: Duration) -> std::io::Result<String> {
                Err(std::io::ErrorKind::ConnectionRefused.into())
            }
            fn endpoint(&self) -> &str {
                "nowhere"
            }
        }
        let gone = || Some(Channel::new(Unreachable));
        sidecar_sets_cutoff(&voice, 1200.0);
        assert!(session.refresh(&gone));
        let deadline = Instant::now() + test_support::SEEN;
        while session.status() == &Status::Swapping {
            assert!(Instant::now() < deadline, "the Swap never came back");
            session.poll(Instant::now(), &gone);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(session.status(), Status::Held(_)));
    }

    #[test]
    fn files_written_without_a_pause_are_still_read() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("no_pause", "default.json");
            let voice = voice_of(&rig);
            let until = Instant::now() + Duration::from_secs(2);
            let mut cutoff = 500.0;
            while Instant::now() < until {
                cutoff += 1.0;
                sidecar_sets_cutoff(&voice, cutoff);
                let pause = Instant::now() + QUIET / 3;
                while Instant::now() < pause {
                    rig.poll();
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            assert!(
                rig.touches() > 0,
                "nothing was read while the writes kept coming"
            );
        });
    }

    #[test]
    fn a_reload_made_by_refresh_is_reported_by_the_next_poll_once() {
        let scratch = test_support::Scratch::new("sticky");
        let lib = scratch.library();
        let voice = lib.join("voices").join("default-voice.json");
        let mut session = Session::open(&lib.join("default.json"), None, || {});
        sidecar_sets_cutoff(&voice, 1200.0);
        assert!(session.refresh(&|| None));
        assert!(session.poll(Instant::now(), &|| None).reloaded);
        assert!(!session.poll(Instant::now(), &|| None).reloaded);
    }
}

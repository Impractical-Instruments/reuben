//! The open instrument as it is on disk: its [`Tree`] of files, the watcher over them, and the
//! Swap that follows every change to any of them.
//!
//! Nothing here holds a document that differs from its file. A change — the editor's own write,
//! `reuben-mcp`'s, a text editor's — is seen by the watcher, the whole tree is read again, and the
//! top-level instrument is swapped in. A tree that does not load is shown and not swapped: the
//! engine keeps playing the last one that did, until the files load again.

mod tree;
mod undo;
mod users;
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

pub use self::tree::{nested, Document, Tree};
pub use self::undo::{Direction, Step, Undo};
use self::users::users;
pub use self::view::{bar, shortcut, trail, TrailAsk};
use self::watch::Watch;
use crate::feedback::Feedback;

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

/// Who plays the viewed nested document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    Checking,
    /// The names of the instruments, sorted, or why they could not be listed.
    Listed(Result<Vec<String>, String>),
}

/// One step from a document down into the document one of its nodes references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    /// The node, in the document one step up.
    pub address: String,
    /// The document the node references, as the session's tree spells it.
    pub source: String,
}

pub struct Session {
    top: PathBuf,
    root: Option<PathBuf>,
    tree: Tree,
    /// The way from the top-level document down to the one being viewed; empty while the
    /// top-level document is.
    trail: Vec<Crumb>,
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
    /// Why the last edit did not land, until one does: kept here so the canvas and the inspector,
    /// which both write, show the same one.
    feedback: Option<Feedback>,
    /// The store each document is written through, kept for the session's life so the states a
    /// step names stay held for a restore.
    histories: BTreeMap<String, DocumentHistory<FsResolver>>,
    /// How many states each of `histories` keeps.
    states: usize,
    /// The file `users` is about, and the instruments that play it, as last listed.
    users: Option<(PathBuf, Listing)>,
    /// Where a listing under way will arrive. Replaced, and so let go, by the next listing.
    listing: Option<Receiver<Result<Vec<String>, String>>>,
    /// The content hash the last of this session's Swaps installed, until taken.
    installed: Option<String>,
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
            trail: Vec::new(),
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
            feedback: None,
            histories: BTreeMap::new(),
            states: DocumentHistory::<FsResolver>::DEFAULT_CAPACITY,
            users: None,
            listing: None,
            installed: None,
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

    pub fn trail(&self) -> &[Crumb] {
        &self.trail
    }

    /// The document being viewed: the top-level one, or the one at the end of the trail.
    pub fn viewed(&self) -> &Document {
        (self.trail.last())
            .and_then(|crumb| self.tree.document(&crumb.source))
            .unwrap_or_else(|| self.tree.top())
    }

    /// View the document the node at `address` of the viewed document references as its voice or
    /// its patch.
    pub fn enter(&mut self, address: &str) -> Result<(), String> {
        let Some(source) = self.viewed().nested(address) else {
            return Err(format!("{address} references no voice or patch"));
        };
        let source = source.to_string();
        if self.tree.document(&source).is_none() {
            return Err(format!("{source} is not a file of the open instrument"));
        }
        self.trail.push(Crumb {
            address: address.to_string(),
            source,
        });
        self.list_users();
        Ok(())
    }

    /// The path the engine's probes name the node at `address` of the viewed document by. A patch
    /// the trail passes through is spliced into the Plan above it, so its node's address prefixes
    /// the next address; a voice is a Plan of its own, hosted by its node, so that node's address
    /// ends one segment and the voice's addresses start the next.
    pub fn probe_path(&self, address: &str) -> Vec<String> {
        let mut path = Vec::new();
        let mut prefix = String::new();
        let mut above = self.tree.top();
        for crumb in &self.trail {
            prefix.push_str(&crumb.address);
            if above.hosts(&crumb.address) {
                path.push(std::mem::take(&mut prefix));
            }
            let Some(below) = self.tree.document(&crumb.source) else {
                break;
            };
            above = below;
        }
        path.push(prefix + address);
        path
    }

    /// View the document `depth` steps down the trail: 0 is the top-level document.
    pub fn leave(&mut self, depth: usize) {
        self.trail.truncate(depth);
        self.list_users();
    }

    /// The instruments that play the viewed document: those beside the top-level file and those
    /// directly in the library root. `None` while the top-level document is viewed.
    pub fn users(&mut self) -> Option<&Listing> {
        if let Some(listed) = self.listing.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.listing = None;
            if let Some((_, listing)) = &mut self.users {
                *listing = Listing::Listed(listed);
            }
        }
        self.users.as_ref().map(|(_, listing)| listing)
    }

    /// List again who plays the viewed document, off the UI thread: it reads every instrument
    /// beside the top-level file and in the root. Every time the view moves or the files change,
    /// since another instrument may have taken up or let go of the file meanwhile. The last answer
    /// for the same file stands until the new one arrives.
    fn list_users(&mut self) {
        self.listing = None;
        if self.trail.is_empty() {
            self.users = None;
            return;
        }
        let path = self.viewed().path.clone();
        let beside = self.tree.top().path.parent().map(Path::to_path_buf);
        let root = self.root.clone();
        let (done, listing) = mpsc::channel();
        let scanned = path.clone();
        let wake = Arc::clone(&self.wake);
        std::thread::spawn(move || {
            let libraries: Vec<&Path> = beside
                .as_deref()
                .into_iter()
                .chain(root.as_deref())
                .collect();
            if done
                .send(users(&libraries, root.as_deref(), &scanned))
                .is_ok()
            {
                wake();
            }
        });
        self.listing = Some(listing);
        if self
            .users
            .as_ref()
            .is_none_or(|(listed, _)| *listed != path)
        {
            self.users = Some((path, Listing::Checking));
        }
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
        let states = self.states;
        self.histories
            .entry(source.to_string())
            .or_insert_with(|| DocumentHistory::with_capacity(tree::store(source, root), states))
    }

    /// Keep `states` states per document from here on, so a test can push one out of the store.
    #[cfg(test)]
    pub fn keep_states(&mut self, states: usize) {
        self.states = states;
        self.histories.clear();
    }

    pub fn undo(&self) -> &Undo {
        &self.undo
    }

    pub fn undo_mut(&mut self) -> &mut Undo {
        &mut self.undo
    }

    pub fn feedback(&self) -> Option<&Feedback> {
        self.feedback.as_ref()
    }

    pub fn feedback_mut(&mut self) -> &mut Option<Feedback> {
        &mut self.feedback
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

    /// The content hash of the top-level file one of this session's Swaps installed since the last
    /// call: known the moment the Swap answers, where asking the engine what it plays waits on its
    /// next answer.
    pub fn take_installed(&mut self) -> Option<String> {
        self.installed.take()
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
        self.trail = followed(&self.tree, std::mem::take(&mut self.trail));
        self.list_users();
        self.changed = true;
        let tree = &self.tree;
        let hash_of = |source: &str| tree.document(source).and_then(|d| d.hash());
        if self.feedback.as_ref().is_some_and(|f| f.outlived(hash_of)) {
            self.feedback = None;
        }
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
        if let Ok(result) = &outcome {
            if result.report.report.ok && result.conflict.is_none() && result.held.is_none() {
                self.installed = Some(result.report.content_hash.clone());
            }
        }
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

/// As much of `trail` as `tree` still leads down: each step's node still in the document above it
/// and still referencing the same document. The rest is let go, the view going back up to the
/// last document still reached.
fn followed(tree: &Tree, trail: Vec<Crumb>) -> Vec<Crumb> {
    let mut at = tree.top();
    let mut kept = Vec::with_capacity(trail.len());
    for crumb in trail {
        let next = (at.nested(&crumb.address))
            .filter(|source| *source == crumb.source)
            .and_then(|source| tree.document(source));
        let Some(next) = next else { break };
        at = next;
        kept.push(crumb);
    }
    kept
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

    use reuben_api::authoring::{new_instrument, DocumentHistory, NewInstrument, Resources};
    use reuben_api::engine::{
        get_current_instrument, install_initial, Channel, InProcess, Request, StructureState,
        Transport,
    };
    use reuben_api::render::AudioConfig;
    use reuben_api::staged::Staged;
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

    /// Write `lib/probed.json`: an oscillator `/osc`, a `subpatch` `/bass` whose patch has a filter
    /// `/filter` feeding its `audio` output, a `subpatch` `/wrap` whose `audio` output comes from
    /// that same patch nested in it as `/inner`, and a `voicer` `/kick_v` whose voice has an
    /// oscillator `/osc` feeding its `audio`. Returns the top-level file.
    pub fn probed(scratch: &Scratch) -> PathBuf {
        let lib = scratch.path("lib");
        std::fs::create_dir_all(&lib).expect("create the library directory");
        let write = |name: &str, doc: serde_json::Value| {
            let path = lib.join(name);
            std::fs::write(&path, doc.to_string()).expect("write a document");
            path
        };
        write(
            "bass.json",
            serde_json::json!({
                "format_version": 3,
                "instrument": "bass",
                "interface": {"outputs": {"audio": {"from": "/filter.audio"}}},
                "nodes": [
                    {"type": "oscillator", "address": "/osc"},
                    {"type": "filter", "address": "/filter", "inputs": {"audio": {"from": "/osc"}}}
                ]
            }),
        );
        write(
            "wrap.json",
            serde_json::json!({
                "format_version": 3,
                "instrument": "wrap",
                "resources": {"bass": "bass.json"},
                "interface": {"outputs": {"audio": {"from": "/inner.audio"}}},
                "nodes": [{"type": "subpatch", "address": "/inner", "patch": "bass"}]
            }),
        );
        write(
            "kick.json",
            serde_json::json!({
                "format_version": 3,
                "instrument": "kick",
                "interface": {
                    "inputs": {"gate": {"type": "f32", "default": 0.0, "min": 0.0, "max": 1.0}},
                    "outputs": {"audio": {"from": "/osc.audio"}}
                },
                "nodes": [{"type": "oscillator", "address": "/osc"}]
            }),
        );
        write(
            "probed.json",
            serde_json::json!({
                "format_version": 3,
                "instrument": "probed",
                "resources": {"bass": "bass.json", "kick": "kick.json", "wrap": "wrap.json"},
                "interface": {"outputs": {"out": {"from": "/mix2.out"}}},
                "nodes": [
                    {"type": "oscillator", "address": "/osc"},
                    {"type": "subpatch", "address": "/bass", "patch": "bass"},
                    {"type": "voicer", "address": "/kick_v", "voice": "kick",
                     "config": {"voices": 1}},
                    {"type": "add_f32_signal", "address": "/mix",
                     "inputs": {"a": {"from": "/bass.audio"}, "b": {"from": "/kick_v"}}},
                    {"type": "subpatch", "address": "/wrap", "patch": "wrap"},
                    {"type": "add_f32_signal", "address": "/mix2",
                     "inputs": {"a": {"from": "/mix"}, "b": {"from": "/wrap.audio"}}}
                ]
            }),
        )
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
            Self::started(scratch, lib, top)
        }

        /// A rig on a new instrument, as `new_instrument` seeds it, at `lib/<name>.json`.
        pub fn fresh(test: &str, name: &str) -> Self {
            let scratch = Scratch::new(test);
            let lib = scratch.path("lib");
            std::fs::create_dir_all(&lib).expect("create the library directory");
            let top = lib.join(format!("{name}.json"));
            let source = top.display().to_string();
            let seeded = new_instrument(
                &NewInstrument {
                    source: source.clone(),
                    name: name.to_string(),
                },
                &FsResolver::for_document(&source),
            )
            .expect("the seed is written")
            .output;
            assert!(seeded.written, "{:?}", seeded.report);
            Self::started(scratch, lib, top)
        }

        fn started(scratch: Scratch, lib: PathBuf, top: PathBuf) -> Self {
            let text = std::fs::read_to_string(&top).expect("read the fixture");
            let staged: Staged<Box<dyn Resources + Send + Sync>> =
                Staged::new(Box::new(FsResolver::for_instrument(&top)));
            let (coordinator, side, _warnings) =
                install_initial(&text, staged.clone(), AudioConfig::new(48_000.0, 128))
                    .expect("install the fixture");
            let (control, control_rx) = mpsc::channel::<ControlBatch>();
            let host = NativeHost::new(Diagnostics::new(), control);
            let state = StructureState::new(
                coordinator,
                Arc::new(host),
                Arc::new(DocumentHistory::new(staged)),
            )
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

        /// Wait for `n` live control messages to reach the render side, and return every one
        /// that has, in order.
        pub fn live_received(&self, n: usize) -> Vec<reuben_native::osc::OscIn> {
            self._callback.await_queued(n)
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
        rename_instrument_node, set_instrument_constant, set_instrument_description,
        set_instrument_input, RenameInstrumentNode, SetInstrumentConstant,
        SetInstrumentDescription, SetInstrumentInput,
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
    fn the_trail_goes_down_through_a_voice_into_its_patch_and_back_up() {
        let scratch = test_support::Scratch::new("trail");
        let lib = scratch.library();
        let mut session = Session::open(&lib.join("acid-techno.json"), None, || {});
        assert_eq!(session.users(), None, "the top-level document is viewed");
        assert!(session.enter("/tempo").is_err(), "no voice or patch there");
        assert!(session.trail().is_empty());

        session.enter("/kick_v").expect("a voicer");
        session.enter("/vca").expect("a subpatch inside the voice");
        assert_eq!(
            session.viewed().path,
            lib.join("voices").join("shaped-vca.json")
        );
        let addresses: Vec<&str> = session.trail().iter().map(|c| c.address.as_str()).collect();
        assert_eq!(addresses, ["/kick_v", "/vca"]);

        session.leave(1);
        assert_eq!(
            session.viewed().path,
            lib.join("voices").join("kick-voice.json")
        );
        session.leave(0);
        assert_eq!(session.viewed().path, session.tree().top().path);
    }

    #[test]
    fn a_probe_path_carries_the_subpatch_prefix_and_splits_at_a_voicer() {
        use reuben_native::test_support::start_headless;

        let scratch = test_support::Scratch::new("probe_path");
        let top = test_support::probed(&scratch);
        let mut session = Session::open(&top, None, || {});
        let top_level = session.probe_path("/osc");
        session.enter("/bass").expect("a subpatch");
        let in_patch = session.probe_path("/filter");
        session.leave(0);
        session.enter("/kick_v").expect("a voicer");
        let in_voice = session.probe_path("/osc");
        assert_eq!(top_level, ["/osc"]);
        assert_eq!(in_patch, ["/bass/filter"]);
        assert_eq!(in_voice, ["/kick_v", "/osc"]);

        // Each names a node of the Plan the engine built from these files.
        within(HANG_SECS, move || {
            let played = start_headless(crate::engine::test_support::config(top))
                .expect("the engine starts");
            let paths = vec![top_level, in_patch, in_voice];
            let resolved = played.channel().set_probes(paths.clone());
            reuben_native::diagnostics::log_snapshot(&played.shutdown());
            assert_eq!(resolved.expect("set_probes"), paths);
        });
    }

    #[test]
    fn the_trail_goes_back_up_to_what_the_files_still_reach() {
        let scratch = test_support::Scratch::new("trail_follows");
        let lib = scratch.library();
        let top = lib.join("acid-techno.json");
        let mut session = Session::open(&top, None, || {});
        session.enter("/kick_v").expect("a voicer");
        session.enter("/vca").expect("a subpatch");

        let source = spelled(&top);
        let store = || FsResolver::for_document(&source).stat_only();
        // An edit that leaves the way down as it was keeps the view.
        let described = set_instrument_description(
            &SetInstrumentDescription {
                source: source.clone(),
                description: Some("moved".to_string()),
                expect: None,
            },
            &store(),
        )
        .expect("the verb ran")
        .output;
        assert!(described.written, "{:?}", described.report);
        assert!(session.refresh(&|| None));
        assert_eq!(session.trail().len(), 2);

        let renamed = rename_instrument_node(
            &RenameInstrumentNode {
                source: source.clone(),
                from: "/kick_v".to_string(),
                to: "/kick_drum".to_string(),
                expect: None,
            },
            &store(),
        )
        .expect("the verb ran")
        .output;
        assert!(renamed.written, "{:?}", renamed.report);
        assert!(session.refresh(&|| None));
        assert!(session.trail().is_empty());
        assert_eq!(session.viewed().path, session.tree().top().path);
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

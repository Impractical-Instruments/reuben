//! The open instrument as the server has it: its [`Tree`] of documents, read in one call, and read
//! again whenever the server says something changed.
//!
//! Nothing here holds a document that differs from what the server read. A change — the editor's
//! own edit, `reuben-mcp`'s, a text editor's write the server followed — arrives as an [`Event`],
//! and the whole tree is read again. The server plays what loads; a tree that does not is shown
//! held, with why.

mod tree;
mod undo;
mod users;
mod view;

use std::collections::BTreeSet;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reuben_api::engine::{Channel, Event};
use reuben_native::tree_watch::LONGEST_WAIT;

pub use self::tree::{nested, Document, Tree};
pub use self::undo::{Direction, Step, Undo};
use self::users::users;
pub use self::view::{bar, shortcut, trail, TrailAsk};
use crate::feedback::Feedback;

/// How long after a read that no event set off, and that found the tree held, it is read once
/// more: past the server's longest debounce of an outside write, plus its reload. A read made
/// inside that debounce can see a write the server then finds was undone, and publishes nothing
/// for.
const RECHECK_AFTER: Duration = Duration::from_millis(LONGEST_WAIT.as_millis() as u64 + 250);

/// What the session last read of the open instrument's tree from the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Every document of the tree reads, and none reports an error. Whether the engine has
    /// installed it yet is not part of this.
    Current,
    /// No engine is running, so there is no server to read from and nothing plays.
    NoEngine,
    /// The tree does not play as it is, and why: a document does not read, or its load reports
    /// errors, or the server could not be asked.
    Held(Vec<String>),
}

/// What one [`Session::poll`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Poll {
    /// The tree was read since the last poll, by it or by [`Session::refresh`], and differed
    /// from the one before.
    pub reloaded: bool,
    /// The name of an instrument the server opened in place of this session's, when one was.
    pub opened: Option<String>,
    /// Poll again no later than this, for a read that is due then even if no event comes.
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
    top: String,
    tree: Tree,
    /// The way from the top-level document down to the one being viewed; empty while the
    /// top-level document is.
    trail: Vec<Crumb>,
    /// The tree changed since [`poll`](Self::poll) last said so.
    changed: bool,
    status: Status,
    /// When to read the tree once more, a read having found it held. See [`RECHECK_AFTER`].
    recheck: Option<Instant>,
    wake: Wake,
    undo: Undo,
    /// Why the last edit did not land, until one does: kept here so the canvas and the inspector,
    /// which both write, show the same one.
    feedback: Option<Feedback>,
    /// The document `users` is about, and the instruments that play it, as last listed.
    users: Option<(String, Listing)>,
    /// A listing is due: the view moved or the tree changed since the last one was asked for.
    relist: bool,
    /// Where a listing under way will arrive. Replaced, and so let go, by the next listing.
    listing: Option<Receiver<Result<Vec<String>, String>>>,
    /// The viewed document and the tree's documents the last listing was asked for. Who plays the
    /// viewed document changes only with another instrument's files, which the server sends no
    /// event for, or with which documents the open tree reads: an edit that leaves both alone asks
    /// for nothing, since each listing reads the server's whole library.
    listed_for: Option<(String, BTreeSet<String>)>,
}

impl Session {
    /// Open the instrument the server names `top`, reading its tree through `channel` now.
    pub fn open(top: &str, channel: &dyn Fn() -> Option<Channel>) -> Self {
        let mut session = Self {
            top: top.to_string(),
            tree: Tree::unread(top, "not read yet".to_string()),
            trail: Vec::new(),
            changed: false,
            status: Status::NoEngine,
            recheck: None,
            wake: Arc::new(|| {}),
            undo: Undo::default(),
            feedback: None,
            users: None,
            relist: false,
            listing: None,
            listed_for: None,
        };
        if channel().is_none() {
            session.tree = Tree::unread(top, "no engine is running to read it".to_string());
        }
        session.refresh(channel);
        session.changed = false;
        session
    }

    /// `wake` is called from another thread whenever a listing of [`users`](Self::users) arrives.
    pub fn with_wake(mut self, wake: impl Fn() + Send + Sync + 'static) -> Self {
        self.wake = Arc::new(wake);
        self
    }

    pub fn top(&self) -> &str {
        &self.top
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
            return Err(format!("{source} is not a document of the open instrument"));
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

    /// The instruments of the server's library that play the viewed document, asked through
    /// `channel` when a listing is due. `None` while the top-level document is viewed.
    pub fn users(&mut self, channel: &dyn Fn() -> Option<Channel>) -> Option<&Listing> {
        if std::mem::take(&mut self.relist) {
            self.ask_users(channel());
        }
        if let Some(listed) = self.listing.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.listing = None;
            if listed.is_err() {
                // Asked again at the next change, whatever it is.
                self.listed_for = None;
            }
            if let Some((_, listing)) = &mut self.users {
                *listing = Listing::Listed(listed);
            }
        }
        self.users.as_ref().map(|(_, listing)| listing)
    }

    /// Have who plays the viewed document listed again, on the next [`users`](Self::users), when
    /// the view moved to another document or the tree reads other documents than at the last
    /// listing (see [`listed_for`](Self::listed_for)). The last answer for the same document stands
    /// until the new one arrives.
    fn list_users(&mut self) {
        if self.trail.is_empty() {
            self.listing = None;
            self.users = None;
            self.relist = false;
            self.listed_for = None;
            return;
        }
        let source = self.viewed().source.clone();
        let documents = (self.tree.documents().iter())
            .map(|document| document.source.clone())
            .collect();
        let asked = (source.clone(), documents);
        if self.listed_for.as_ref() == Some(&asked) {
            return;
        }
        self.listed_for = Some(asked);
        self.listing = None;
        if (self.users.as_ref()).is_none_or(|(listed, _)| *listed != source) {
            self.users = Some((source, Listing::Checking));
        }
        self.relist = true;
    }

    /// Ask the server's library, off the UI thread, which instruments read the viewed document.
    fn ask_users(&mut self, channel: Option<Channel>) {
        let Some((source, _)) = &self.users else {
            return;
        };
        let source = source.clone();
        let (done, listing) = mpsc::channel();
        let wake = Arc::clone(&self.wake);
        std::thread::spawn(move || {
            let listed = match channel {
                Some(channel) => (channel.library())
                    .map(|library| users(&library, &source))
                    .map_err(|e| e.to_string()),
                None => Err("no engine is running to list the library".to_string()),
            };
            if done.send(listed).is_ok() {
                wake();
            }
        });
        self.listing = Some(listing);
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn undo(&self) -> &Undo {
        &self.undo
    }

    /// Drop the undo and redo stacks, the note saying `why` when there was a step to drop.
    pub fn forget_undo(&mut self, why: &str) {
        if self.undo.next(Direction::Undo).is_none() && self.undo.next(Direction::Redo).is_none() {
            return;
        }
        self.undo = Undo::default();
        self.undo.set_note(Some(why.to_string()));
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

    /// Take in the server's `events` since the last call: any at all, of whatever kind, is one
    /// read of the tree, since an event does not say which document changed. With none, a read
    /// that came due is made.
    pub fn poll(&mut self, events: &[Event], channel: &dyn Fn() -> Option<Channel>) -> Poll {
        let opened = (events.iter().rev())
            .find_map(|event| match event {
                Event::Opened { name, .. } => Some(name),
                _ => None,
            })
            .filter(|name| !name.is_empty() && **name != self.top)
            .cloned();
        let now = Instant::now();
        if !events.is_empty() {
            self.refresh(channel);
        } else if self.recheck.is_some_and(|due| now >= due) {
            self.recheck = None;
            self.read(channel);
        }
        Poll {
            reloaded: std::mem::take(&mut self.changed),
            opened,
            wait: self.recheck.map(|due| due.saturating_duration_since(now)),
        }
    }

    /// Read the tree from the server now. Returns whether it differed from the one held.
    ///
    /// With no engine running the tree is kept, and the status says nothing plays. A read the
    /// server refused, or that did not reach it, keeps the tree too, and holds with why. A read
    /// that holds has another come due after [`RECHECK_AFTER`], which [`poll`](Self::poll) makes.
    pub fn refresh(&mut self, channel: &dyn Fn() -> Option<Channel>) -> bool {
        let changed = self.read(channel);
        self.recheck =
            matches!(self.status, Status::Held(_)).then(|| Instant::now() + RECHECK_AFTER);
        changed
    }

    fn read(&mut self, channel: &dyn Fn() -> Option<Channel>) -> bool {
        let Some(channel) = channel() else {
            self.status = Status::NoEngine;
            return false;
        };
        let tree = match Tree::read(&self.top, &channel) {
            Ok(tree) => tree,
            Err(why) => {
                self.status = Status::Held(vec![why]);
                return false;
            }
        };
        let faults = tree.faults();
        self.status = match faults.is_empty() {
            true => Status::Current,
            false => Status::Held(faults),
        };
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
        true
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

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use reuben_api::authoring::{new_instrument, DocumentHistory, NewInstrument, Resources};
    use reuben_api::engine::{
        install_initial, Channel, Event, InProcess, Request, StructureState, Transport,
    };
    use reuben_api::render::AudioConfig;
    use reuben_api::staged::Staged;
    use reuben_api::FsResolver;
    use reuben_native::control::ring_pair;
    use reuben_native::diagnostics::Diagnostics;
    use reuben_native::structure::NativeHost;
    use reuben_native::test_support::FakeCallback;
    use reuben_native::tree_watch::{self, TreeWatch};

    use super::{Session, Status};
    use crate::follow::Follow;

    type Store = Staged<Box<dyn Resources + Send + Sync>>;

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

    /// Whether the server's name `source` names the file at `path`, however either is spelled.
    pub fn names(source: &str, path: &Path) -> bool {
        let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        canonical(Path::new(source)) == canonical(path)
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

    /// What [`Rig::calls`] counts `library` requests under.
    pub const LIBRARY: &str = "<library>";

    /// How many `call`s of each tool, and `library` requests, have crossed a [`Counting`]
    /// transport.
    type Calls = Arc<Mutex<BTreeMap<String, usize>>>;

    /// The engine's own transport, counting the `call`s that cross it by tool name.
    #[derive(Debug)]
    struct Counting {
        inner: InProcess,
        calls: Calls,
    }

    impl Transport for Counting {
        fn round_trip(&self, line: &str, read_timeout: Duration) -> std::io::Result<String> {
            match Request::from_ndjson(line) {
                Ok(Request::Call { tool, .. }) => {
                    *self.calls.lock().unwrap().entry(tool).or_default() += 1;
                }
                Ok(Request::Library) => {
                    *(self.calls.lock().unwrap())
                        .entry(LIBRARY.to_string())
                        .or_default() += 1;
                }
                _ => {}
            }
            self.inner.round_trip(line, read_timeout)
        }

        fn endpoint(&self) -> &str {
            self.inner.endpoint()
        }

        fn subscribe(
            &self,
        ) -> std::io::Result<Box<dyn Iterator<Item = std::io::Result<Event>> + Send>> {
            Transport::subscribe(&self.inner)
        }
    }

    /// A session on a scratch copy of a library instrument, and an engine playing that copy with
    /// no audio device: the Coordinator, host, window and file watcher are the ones `reuben play`
    /// wires up, and a [`FakeCallback`] renders in place of the device. Built from parts rather
    /// than started, so the transport the session reaches it through can count its calls, and the
    /// server's history can be sized. Every edit the session makes is a `call` to this server, and
    /// every write from outside is followed by the server and heard through [`Follow`].
    pub struct Rig {
        pub lib: PathBuf,
        pub top: PathBuf,
        pub session: Session,
        state: StructureState,
        calls: Calls,
        follow: Follow,
        _watch: TreeWatch,
        _callback: FakeCallback,
        _scratch: Scratch,
    }

    /// How long a change may take to reach the engine before a test calls it lost.
    pub const SEEN: Duration = Duration::from_secs(10);
    /// How long a test keeps polling for a change that must not come. Many times the server's
    /// quiet period plus the watcher's latency.
    pub const SETTLED: Duration = Duration::from_millis(600);

    impl Rig {
        pub fn open(test: &str, instrument: &str) -> Self {
            Self::keeping(test, instrument, DocumentHistory::<Store>::DEFAULT_CAPACITY)
        }

        /// [`open`](Self::open), on a server whose history holds `states` documents, so a test
        /// can push a state out of it.
        pub fn keeping(test: &str, instrument: &str, states: usize) -> Self {
            let scratch = Scratch::new(test);
            let lib = scratch.library();
            let top = lib.join(instrument);
            Self::started(scratch, lib, top, states)
        }

        /// A rig on the instrument at `top`, which `scratch` holds, written by the test.
        pub fn on(scratch: Scratch, top: PathBuf) -> Self {
            let lib = top.parent().expect("a file in a directory").to_path_buf();
            Self::started(
                scratch,
                lib,
                top,
                DocumentHistory::<Store>::DEFAULT_CAPACITY,
            )
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
            Self::started(
                scratch,
                lib,
                top,
                DocumentHistory::<Store>::DEFAULT_CAPACITY,
            )
        }

        fn started(scratch: Scratch, lib: PathBuf, top: PathBuf, states: usize) -> Self {
            let text = std::fs::read_to_string(&top).expect("read the fixture");
            let staged: Store = Staged::new(Box::new(FsResolver::for_instrument(&top)));
            let (coordinator, side, _warnings) =
                install_initial(&text, staged.clone(), AudioConfig::new(48_000.0, 128))
                    .expect("install the fixture");
            let (control, control_rx) = ring_pair(Diagnostics::new());
            let host = NativeHost::new(Diagnostics::new(), control);
            let state = StructureState::new(
                coordinator,
                Arc::new(host),
                Arc::new(DocumentHistory::with_capacity(staged, states)),
            )
            .with_installed_source(Some(top.display().to_string()));
            let calls = Calls::default();
            let channel = channel_of(&state, &calls);
            let follow = Follow::start(channel.clone(), || {});
            // Subscribed before the rig is handed over, so no write a test makes goes unheard. The
            // event a new subscription starts with is let go: the session has just read the tree,
            // and a read it set off later would see a test's write before the server had.
            let deadline = Instant::now() + SEEN;
            while follow.drain().is_empty() {
                assert!(Instant::now() < deadline, "the follow never subscribed");
                std::thread::sleep(Duration::from_millis(1));
            }
            Rig {
                session: Session::open(&top.display().to_string(), &channel),
                lib,
                top,
                _watch: tree_watch::spawn(state.clone()),
                state,
                calls,
                follow,
                _callback: FakeCallback::spawn(side, control_rx),
                _scratch: scratch,
            }
        }

        pub fn channel(&self) -> Channel {
            channel_of(&self.state, &self.calls)().expect("the rig's engine runs")
        }

        /// The server's name for the top-level document.
        pub fn top_source(&self) -> String {
            self.top.display().to_string()
        }

        /// How many `call`s of `tool` have reached the engine through this rig's channels.
        pub fn calls(&self, tool: &str) -> usize {
            self.calls.lock().unwrap().get(tool).copied().unwrap_or(0)
        }

        /// The content hash of what the engine is playing; empty while nothing does.
        pub fn playing(&self) -> String {
            self.channel()
                .get_document()
                .expect("the engine answers")
                .content_hash
        }

        /// What the session asks for the engine's channel.
        pub fn channel_source(&self) -> impl Fn() -> Option<Channel> + Send + Clone + 'static {
            channel_of(&self.state, &self.calls)
        }

        /// Take in the events the server sent since the last poll. Returns whether the tree was
        /// read again and differed.
        pub fn poll(&mut self) -> bool {
            let channel = self.channel_source();
            let events = self.follow.drain();
            self.session.poll(&events, &channel).reloaded
        }

        /// Wait until the server has sent `n` events since the last poll, and return them
        /// without taking them in.
        pub fn events(&self, n: usize) -> Vec<Event> {
            let deadline = Instant::now() + SEEN;
            let mut events = Vec::new();
            while events.len() < n {
                assert!(
                    Instant::now() < deadline,
                    "{} of {n} events came",
                    events.len()
                );
                events.extend(self.follow.drain());
                std::thread::sleep(Duration::from_millis(1));
            }
            events
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
        }

        /// Wait for `n` live control messages to reach the render side, and return every one
        /// that has, in order.
        pub fn live_received(&self, n: usize) -> Vec<reuben_native::osc::OscIn> {
            self._callback.await_queued(n)
        }

        pub fn status(&self) -> &Status {
            self.session.status()
        }
    }

    impl Drop for Rig {
        /// Ends every subscription, so the rig's [`Follow`] thread goes with it.
        fn drop(&mut self) {
            self.state.close();
        }
    }

    fn channel_of(
        state: &StructureState,
        calls: &Calls,
    ) -> impl Fn() -> Option<Channel> + Send + Clone + 'static {
        let engine = InProcess::new(state.clone());
        let calls = Arc::clone(calls);
        move || {
            Some(Channel::new(Counting {
                inner: engine.clone(),
                calls: Arc::clone(&calls),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use reuben_api::authoring::{
        EditResult, LayoutPosition, LayoutTarget, RenameInstrumentNode, SetInstrumentDescription,
        SetInstrumentInput, SetInstrumentLayout, WireInstrumentInput,
    };
    use reuben_api::engine::{call_tool, TcpTransport, DEFAULT_CONNECT_TIMEOUT};
    use reuben_api::tools::names;
    use reuben_native::engine::{EngineConfig, RunningEngine};
    use reuben_native::test_support::{start_headless, within};
    use serde_json::json;

    use super::test_support::{Rig, Scratch, SEEN};
    use super::*;
    use crate::edit::{self, Verb};
    use crate::follow::Follow;

    const HANG_SECS: u64 = 60;

    /// A write another client of the server makes, as `reuben-mcp` does: `tool` with `args`,
    /// through its own channel. Returns what it wrote.
    fn client_writes(rig: &Rig, tool: &str, args: serde_json::Value) -> EditResult {
        let answer = call_tool(tool, &args, &rig.channel()).expect("the verb ran");
        let edit: EditResult = serde_json::from_value(answer.output).expect("an edit");
        assert!(edit.written, "{:?}", edit.report);
        edit
    }

    fn client_sets_cutoff(rig: &Rig, source: &str, cutoff: f64) -> String {
        let args = SetInstrumentInput {
            source: source.to_string(),
            address: "/filter".to_string(),
            input: "cutoff".to_string(),
            value: json!(cutoff),
            expect: None,
        };
        client_writes(rig, names::SET_INSTRUMENT_INPUT, json!(args)).hash
    }

    /// The server's name for the document of the session's tree whose file ends with `file`.
    fn source_of(rig: &Rig, file: &str) -> String {
        (rig.session.tree().documents().iter())
            .find(|d| Path::new(&d.source).ends_with(file))
            .unwrap_or_else(|| panic!("{file} is in the tree"))
            .source
            .clone()
    }

    fn hash_of(rig: &Rig, source: &str) -> Option<String> {
        (rig.session.tree().document(source)).and_then(|d| d.hash().map(str::to_string))
    }

    /// A document cut off mid-node: it does not parse.
    const BROKEN: &str = r#"{"format_version":3,"instrument":"t","nodes":[{"#;

    const VOICE: &str = "voices/default-voice.json";

    #[test]
    fn the_trail_goes_down_through_a_voice_into_its_patch_and_back_up() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("trail", "acid-techno.json");
            let session = &mut rig.session;
            assert_eq!(
                session.users(&|| None),
                None,
                "the top-level document is viewed"
            );
            assert!(session.enter("/tempo").is_err(), "no voice or patch there");
            assert!(session.trail().is_empty());

            session.enter("/kick_v").expect("a voicer");
            session.enter("/vca").expect("a subpatch inside the voice");
            assert!(Path::new(&session.viewed().source).ends_with("voices/shaped-vca.json"));
            let addresses: Vec<&str> = session.trail().iter().map(|c| c.address.as_str()).collect();
            assert_eq!(addresses, ["/kick_v", "/vca"]);

            session.leave(1);
            assert!(Path::new(&session.viewed().source).ends_with("voices/kick-voice.json"));
            session.leave(0);
            assert_eq!(session.viewed().source, session.top());
        });
    }

    #[test]
    fn a_probe_path_carries_the_subpatch_prefix_and_splits_at_a_voicer() {
        within(HANG_SECS, || {
            let scratch = Scratch::new("probe_path");
            let top = test_support::probed(&scratch);
            let mut rig = Rig::on(scratch, top.clone());
            let session = &mut rig.session;
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
            let played = start_headless(crate::engine::test_support::config(top))
                .expect("the engine starts");
            let paths = vec![top_level, in_patch, in_voice];
            let resolved = played.channel().set_probes(paths.clone());
            reuben_native::diagnostics::log_snapshot(&played.shutdown());
            assert_eq!(resolved.expect("set_probes"), paths);
        });
    }

    #[test]
    fn the_trail_goes_back_up_to_what_the_tree_still_reaches() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("trail_follows", "acid-techno.json");
            rig.session.enter("/kick_v").expect("a voicer");
            rig.session.enter("/vca").expect("a subpatch");
            let source = rig.top_source();
            let channel = rig.channel_source();

            // An edit that leaves the way down as it was keeps the view.
            let described = SetInstrumentDescription {
                source: source.clone(),
                description: Some("moved".to_string()),
                expect: None,
            };
            client_writes(&rig, names::SET_INSTRUMENT_DESCRIPTION, json!(described));
            assert!(rig.session.refresh(&channel));
            assert_eq!(rig.session.trail().len(), 2);

            let renamed = RenameInstrumentNode {
                source: source.clone(),
                from: "/kick_v".to_string(),
                to: "/kick_drum".to_string(),
                expect: None,
            };
            client_writes(&rig, names::RENAME_INSTRUMENT_NODE, json!(renamed));
            assert!(rig.session.refresh(&channel));
            assert!(rig.session.trail().is_empty());
            assert_eq!(rig.session.viewed().source, rig.session.top());
        });
    }

    #[test]
    fn a_client_write_to_a_voice_is_followed_and_the_top_level_plays_on() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("voice_write", "default.json");
            let voice = source_of(&rig, VOICE);
            let playing = rig.playing();

            let written = client_sets_cutoff(&rig, &voice, 1200.0);
            rig.poll_until("the voice read again", |rig| {
                hash_of(rig, &voice).as_deref() == Some(written.as_str())
            });
            assert_eq!(rig.status(), &Status::Current);
            assert_eq!(
                rig.playing(),
                playing,
                "the top-level document is the same one; only its voice moved"
            );
        });
    }

    /// Listed once, then not again for an edit that leaves the tree reading the same documents:
    /// who plays the viewed one cannot have changed. Listed again once the tree reads another.
    #[test]
    fn who_plays_a_viewed_document_is_listed_again_only_when_the_trees_documents_change() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("users_coalesced", "default.json");
            let voice = source_of(&rig, VOICE);
            let channel = rig.channel_source();
            rig.session.enter("/voicer").expect("a voicer");
            let deadline = Instant::now() + SEEN;
            while rig.session.users(&channel) == Some(&Listing::Checking) {
                assert!(Instant::now() < deadline, "never listed");
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(rig.calls(test_support::LIBRARY), 1);

            let written = client_sets_cutoff(&rig, &voice, 1200.0);
            rig.poll_until("the voice read again", |rig| {
                hash_of(rig, &voice).as_deref() == Some(written.as_str())
            });
            rig.session.users(&channel);
            rig.settle();
            rig.session.users(&channel);
            assert_eq!(rig.calls(test_support::LIBRARY), 1, "listed again");

            // From outside, as a text editor writes: one more document in the tree.
            let mut doc: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&rig.top).unwrap()).unwrap();
            doc["resources"]["space"] = json!("patches/space.json");
            (doc["nodes"].as_array_mut().unwrap())
                .push(json!({"type": "subpatch", "address": "/space", "patch": "space"}));
            let staged = rig.top.with_extension("json.tmp");
            std::fs::write(&staged, doc.to_string()).unwrap();
            std::fs::rename(&staged, &rig.top).unwrap();
            rig.poll_until("the new resource read", |rig| {
                rig.session.tree().documents().len() > 2
            });
            rig.session.users(&channel);
            let deadline = Instant::now() + SEEN;
            while rig.calls(test_support::LIBRARY) < 2 {
                assert!(Instant::now() < deadline, "not listed again");
                std::thread::sleep(Duration::from_millis(5));
            }
        });
    }

    #[test]
    fn a_client_write_to_the_top_level_document_is_followed_and_plays() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("top_write", "default.json");
            let before = rig.playing();
            let source = rig.top_source();
            let edit = client_writes(
                &rig,
                names::SET_INSTRUMENT_CONSTANT,
                json!({"source": source, "address": "/voicer", "name": "voices", "value": 4}),
            );
            rig.poll_until("the top read again", |rig| {
                rig.session.tree().top().hash() == Some(edit.hash.as_str())
            });
            assert_eq!(rig.status(), &Status::Current);
            assert_ne!(edit.hash, before);
            assert_eq!(rig.playing(), edit.hash);
        });
    }

    #[test]
    fn an_outside_write_to_a_nested_document_is_followed() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("outside_voice", "default.json");
            let voice = source_of(&rig, VOICE);
            let before = hash_of(&rig, &voice);
            let file = rig.lib.join(VOICE);
            let text = std::fs::read_to_string(&file).unwrap();
            let changed = text.replace("\"cutoff\": 3000.0", "\"cutoff\": 1234.0");
            assert_ne!(changed, text, "the voice has the cutoff to change");
            std::fs::write(&file, changed).unwrap();

            rig.poll_until("the voice read again", |rig| hash_of(rig, &voice) != before);
            assert_eq!(rig.status(), &Status::Current);
        });
    }

    #[test]
    fn an_outside_break_holds_and_its_fix_plays() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("outside_break", "default.json");
            let playing = rig.playing();
            for (file, what) in [
                (rig.lib.join(VOICE), "the voice"),
                (rig.top.clone(), "the top"),
            ] {
                let was = std::fs::read_to_string(&file).unwrap();
                std::fs::write(&file, BROKEN).unwrap();
                rig.poll_until(what, |rig| matches!(rig.status(), Status::Held(_)));
                assert_eq!(rig.playing(), playing, "{what}: what played plays on");

                std::fs::write(&file, was).unwrap();
                rig.poll_until(what, |rig| rig.status() == &Status::Current);
                assert_eq!(rig.playing(), playing, "{what}");
            }
        });
    }

    #[test]
    fn opening_a_broken_document_shows_it_held_and_the_fixing_edit_plays() {
        within(HANG_SECS, || {
            let rig = Rig::open("open_broken", "default.json");
            let broken = rig.lib.join("broken.json");
            let text = std::fs::read_to_string(&rig.top)
                .unwrap()
                .replace("/voicer.audio", "/nothing.audio");
            std::fs::write(&broken, text).unwrap();
            let name = broken.display().to_string();
            rig.channel()
                .open(name.clone(), None)
                .expect("the open reached the server");

            let channel = rig.channel_source();
            let mut session = Session::open(&name, &channel);
            let Status::Held(why) = session.status() else {
                panic!("{:?}", session.status());
            };
            assert!(why.iter().any(|w| w.contains("/out")), "{why:?}");
            assert_eq!(rig.playing(), "", "nothing plays");

            let fixed = edit::write(&mut session, &channel, &name, |expect| {
                Verb::of(
                    names::WIRE_INSTRUMENT_INPUT,
                    &WireInstrumentInput {
                        source: name.clone(),
                        address: "/out".to_string(),
                        input: "audio".to_string(),
                        from: "/voicer.audio".to_string(),
                        expect,
                    },
                )
            })
            .expect("the verb ran");
            assert!(fixed.written, "{:?}", fixed.report);
            assert_eq!(session.status(), &Status::Current);
            assert_eq!(rig.playing(), fixed.hash);
        });
    }

    #[test]
    fn a_burst_of_events_is_one_tree_read() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("burst", "default.json");
            rig.settle();
            let source = rig.top_source();
            let mut last = String::new();
            for x in [10.0, 20.0, 30.0] {
                let layout = SetInstrumentLayout {
                    source: source.clone(),
                    positions: vec![LayoutPosition {
                        target: LayoutTarget::Node("/voicer".to_string()),
                        x,
                        y: 0.0,
                    }],
                    expect: None,
                };
                last = client_writes(&rig, names::SET_INSTRUMENT_LAYOUT, json!(layout)).hash;
            }
            let events = rig.events(3);
            assert_eq!(events.len(), 3, "{events:?}");
            let reads = rig.calls(names::READ_INSTRUMENT_TREE);
            let channel = rig.channel_source();
            assert!(
                rig.session.poll(&events, &channel).reloaded,
                "the tree changed"
            );
            assert_eq!(rig.calls(names::READ_INSTRUMENT_TREE), reads + 1);
            assert_eq!(rig.session.tree().top().hash(), Some(last.as_str()));
        });
    }

    /// The server debounces a write undone at once into no change, and says nothing. A read made
    /// between the two writes saw the first, and is read again once the debounce is past.
    #[test]
    fn a_held_read_of_a_write_the_server_never_reports_is_read_again() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("transient", "default.json");
            let channel = rig.channel_source();
            let file = rig.lib.join(VOICE);
            let was = std::fs::read_to_string(&file).unwrap();
            std::fs::write(&file, BROKEN).unwrap();
            rig.session.refresh(&channel);
            std::fs::write(&file, was).unwrap();
            assert!(
                matches!(rig.status(), Status::Held(_)),
                "{:?}",
                rig.status()
            );
            let poll = rig.session.poll(&[], &channel);
            assert!(poll.wait.is_some(), "a read is due");
            rig.poll_until("read again", |rig| rig.status() == &Status::Current);
        });
    }

    #[test]
    fn nothing_is_read_while_nothing_changes() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("idle", "acid-techno.json");
            rig.settle();
            let reads = rig.calls(names::READ_INSTRUMENT_TREE);
            rig.settle();
            assert_eq!(rig.calls(names::READ_INSTRUMENT_TREE), reads);
            assert_eq!(rig.status(), &Status::Current);
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
            let old = rig.lib.join(VOICE);
            let new = rig.lib.join("elsewhere").join("moved-voice.json");
            std::fs::create_dir_all(new.parent().unwrap()).unwrap();
            std::fs::copy(&old, &new).unwrap();

            let text = std::fs::read_to_string(&rig.top)
                .unwrap()
                .replace("voices/default-voice.json", "elsewhere/moved-voice.json");
            std::fs::write(&rig.top, text).unwrap();
            rig.poll_until("the new voice in the tree", |rig| {
                (rig.session.tree().documents().iter())
                    .any(|d| Path::new(&d.source).ends_with("elsewhere/moved-voice.json"))
            });
            assert_eq!(rig.session.tree().documents().len(), 2);
            rig.settle();

            // The document that left the tree is no longer the instrument's: the server follows
            // the one that joined, in a directory nothing watched before.
            let reads = rig.calls(names::READ_INSTRUMENT_TREE);
            let text = std::fs::read_to_string(&old).unwrap();
            std::fs::write(&old, text.replace("3000.0", "900.0")).unwrap();
            rig.settle();
            assert_eq!(rig.calls(names::READ_INSTRUMENT_TREE), reads);
            let moved = source_of(&rig, "elsewhere/moved-voice.json");
            let before = hash_of(&rig, &moved);
            let text = std::fs::read_to_string(&new).unwrap();
            std::fs::write(&new, text.replace("3000.0", "900.0")).unwrap();
            rig.poll_until("the moved voice read again", |rig| {
                hash_of(rig, &moved) != before
            });
        });
    }

    #[test]
    fn a_voice_file_that_goes_missing_is_held_until_it_is_back() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("voice_gone", "default.json");
            let voice = rig.lib.join(VOICE);
            let kept = rig.lib.join("kept.json");
            std::fs::rename(&voice, &kept).unwrap();
            rig.poll_until("held", |rig| matches!(rig.status(), Status::Held(_)));

            std::fs::rename(&kept, &voice).unwrap();
            rig.poll_until("back", |rig| rig.status() == &Status::Current);
        });
    }

    #[test]
    fn a_directory_removed_and_made_again_is_followed_afterwards() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("dir_gone", "default.json");
            let voice = rig.lib.join(VOICE);
            let voices = voice.parent().unwrap().to_path_buf();
            let kept = rig.lib.join("kept-voices");
            std::fs::rename(&voices, &kept).unwrap();
            rig.poll_until("held", |rig| matches!(rig.status(), Status::Held(_)));

            // A new directory with the same files, as a checkout or a sync leaves.
            std::fs::create_dir(&voices).unwrap();
            for entry in std::fs::read_dir(&kept).unwrap() {
                let entry = entry.unwrap();
                std::fs::copy(entry.path(), voices.join(entry.file_name())).unwrap();
            }
            rig.poll_until("back", |rig| rig.status() == &Status::Current);

            let source = source_of(&rig, VOICE);
            let before = hash_of(&rig, &source);
            let text = std::fs::read_to_string(&voice).unwrap();
            std::fs::write(&voice, text.replace("3000.0", "900.0")).unwrap();
            rig.poll_until("a write in the new directory", |rig| {
                hash_of(rig, &source) != before
            });
        });
    }

    #[test]
    fn with_no_engine_the_session_says_nothing_plays() {
        let session = Session::open("/nowhere/default.json", &|| None);
        assert_eq!(session.status(), &Status::NoEngine);
        assert!(session.tree().top().read.is_err());
        assert_eq!(session.tree().top().name(), "default");
    }

    #[test]
    fn a_tree_the_engine_cannot_be_reached_for_is_held() {
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
        let session = Session::open("/nowhere/default.json", &gone);
        assert!(matches!(session.status(), Status::Held(_)));
    }

    #[test]
    fn a_reload_made_by_refresh_is_reported_by_the_next_poll_once() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("sticky", "default.json");
            let voice = source_of(&rig, VOICE);
            let channel = rig.channel_source();
            client_sets_cutoff(&rig, &voice, 1200.0);
            assert!(rig.session.refresh(&channel));
            assert!(rig.session.poll(&[], &channel).reloaded);
            assert!(!rig.session.poll(&[], &channel).reloaded);
        });
    }

    #[test]
    fn an_opened_event_for_another_instrument_is_reported() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("opened", "default.json");
            let channel = rig.channel_source();
            let top = rig.top_source();
            let other = Event::Opened {
                name: "/elsewhere/other.json".to_string(),
                hash: String::new(),
            };
            let same = Event::Opened {
                name: top,
                hash: String::new(),
            };
            let poll = rig.session.poll(&[other], &channel);
            assert_eq!(poll.opened.as_deref(), Some("/elsewhere/other.json"));
            assert_eq!(rig.session.poll(&[same], &channel).opened, None);
        });
    }

    /// An engine serving `top` over TCP on `addr`, as `reuben play` does.
    fn served(top: &Path, addr: &str) -> RunningEngine {
        start_headless(EngineConfig {
            structure: Some(addr.to_string()),
            ..crate::engine::test_support::config(top.to_path_buf())
        })
        .expect("the engine starts")
    }

    fn dialed(addr: String) -> impl Fn() -> Option<Channel> + Send + Clone + 'static {
        move || {
            Some(Channel::new(TcpTransport::new(
                &addr,
                DEFAULT_CONNECT_TIMEOUT,
            )))
        }
    }

    #[test]
    fn follow_reconnects_after_the_engine_returns() {
        within(HANG_SECS, || {
            let scratch = Scratch::new("follow_reconnects");
            let lib = scratch.library();
            let top = lib.join("default.json");
            let played = served(&top, "127.0.0.1:0");
            let addr = played.structure_addr().expect("bound").to_string();
            let channel = dialed(addr.clone());
            let follow = Follow::start(channel.clone(), || {});
            let mut session = Session::open(&top.display().to_string(), &channel);
            let before = session.tree().top().hash().map(str::to_string);

            reuben_native::diagnostics::log_snapshot(&played.shutdown());
            // Changed while nothing serves it, so no event says so.
            let text = std::fs::read_to_string(&top).unwrap();
            std::fs::write(&top, text.replace("\"default\"", "\"returned\"")).unwrap();
            let played = served(&top, &addr);

            let deadline = Instant::now() + SEEN;
            while session.tree().top().hash().map(str::to_string) == before {
                assert!(Instant::now() < deadline, "never read again");
                session.poll(&follow.drain(), &channel);
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(session.tree().top().name(), "returned");
            assert_eq!(session.status(), &Status::Current);
            reuben_native::diagnostics::log_snapshot(&played.shutdown());
        });
    }

    #[test]
    fn an_edit_while_the_engine_is_gone_is_refused_promptly() {
        within(HANG_SECS, || {
            let scratch = Scratch::new("edit_engine_gone");
            let lib = scratch.library();
            let top = lib.join("default.json");
            let played = served(&top, "127.0.0.1:0");
            let addr = played.structure_addr().expect("bound").to_string();
            let channel = dialed(addr);
            let source = top.display().to_string();
            let mut session = Session::open(&source, &channel);
            let before = std::fs::read(&top).unwrap();
            reuben_native::diagnostics::log_snapshot(&played.shutdown());

            let asked = Instant::now();
            let edit = edit::write(&mut session, &channel, &source, |expect| {
                Verb::of(
                    names::SET_INSTRUMENT_DESCRIPTION,
                    &SetInstrumentDescription {
                        source: source.clone(),
                        description: Some("lost".to_string()),
                        expect,
                    },
                )
            });
            assert!(edit.is_err(), "{edit:?}");
            assert!(
                asked.elapsed() < Duration::from_secs(3),
                "refused after {:?}",
                asked.elapsed()
            );
            assert!(session.feedback().is_some());
            assert!(matches!(session.status(), Status::Held(_)));
            assert_eq!(std::fs::read(&top).unwrap(), before);
        });
    }

    /// The engine's library root is a directory the editor is never told: not its cwd, not an
    /// argument, not a path it is handed. Every name the editor uses below came from the server.
    #[test]
    fn an_attached_editor_edits_an_engine_rooted_where_it_cannot_see() {
        within(HANG_SECS, || {
            let root = Scratch::new("rooted_elsewhere");
            let library = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../instruments");
            let voices = root.path("voices");
            std::fs::create_dir_all(&voices).unwrap();
            for file in ["default.json", "voices/default-voice.json"] {
                std::fs::copy(library.join(file), root.path(file)).unwrap();
            }
            let played = start_headless(EngineConfig {
                osc_in: None,
                structure: Some("127.0.0.1:0".to_string()),
                instrument_root: voices.parent().map(Path::to_path_buf),
                ..EngineConfig::new(reuben_native::engine::Instrument::Silent)
            })
            .expect("the engine starts");
            let addr = played.structure_addr().expect("bound").to_string();

            let mut engine = crate::engine::Engine::attach(addr, None, || {});
            let channel = || engine.channel();
            let listed = channel().expect("attached").library().expect("the library");
            let top = (listed.instruments.iter())
                .map(|entry| entry.name.clone())
                .find(|name| name.ends_with("default.json"))
                .unwrap_or_else(|| panic!("the instrument is listed: {listed:?}"));
            let opened = channel().unwrap().open(top.clone(), None).expect("open");
            assert!(
                matches!(opened, reuben_api::engine::SwapOutcome::Installed(_)),
                "{opened:?}"
            );

            let (follow, latest) = crate::follower(channel(), || {});
            let mut session = Session::open(&top, &channel);
            assert_eq!(session.status(), &Status::Current);
            let first = session.tree().top().hash().expect("read").to_string();
            let voice = session.tree().documents()[1].source.clone();

            edit::write(&mut session, &channel, &top, |expect| {
                Verb::of(
                    names::SET_INSTRUMENT_DESCRIPTION,
                    &SetInstrumentDescription {
                        source: top.clone(),
                        description: Some("edited from afar".to_string()),
                        expect,
                    },
                )
            })
            .expect("the edit ran");
            let read = session.tree().top().read.as_ref().expect("read");
            assert_eq!(read.description.as_deref(), Some("edited from afar"));
            assert_ne!(session.tree().top().hash(), Some(first.as_str()));

            // Changed by another program, as an outside editor would.
            let before = session
                .tree()
                .document(&voice)
                .unwrap()
                .hash()
                .map(str::to_string);
            let file = root.path("voices/default-voice.json");
            let text = std::fs::read_to_string(&file).unwrap();
            std::fs::write(&file, text.replace("3000.0", "900.0")).unwrap();
            let deadline = Instant::now() + SEEN;
            while session
                .tree()
                .document(&voice)
                .unwrap()
                .hash()
                .map(str::to_string)
                == before
            {
                assert!(
                    Instant::now() < deadline,
                    "the outside change was never followed"
                );
                *latest.lock().unwrap() = channel();
                session.poll(&follow.drain(), &channel);
                std::thread::sleep(Duration::from_millis(10));
            }

            let undone = edit::restore(&mut session, &channel, Direction::Undo);
            assert!(matches!(undone, edit::Restored::Landed), "{undone:?}");
            let answer = channel()
                .unwrap()
                .call(names::READ_INSTRUMENT_TREE, json!({ "source": top }))
                .expect("the server answers");
            let reuben_api::engine::CallOutcome::Called { output, .. } = answer else {
                panic!("refused: {answer:?}");
            };
            assert_eq!(output["documents"][0]["structure"]["hash"], json!(first));
            assert_eq!(session.tree().top().hash(), Some(first.as_str()));

            drop(follow);
            engine.shutdown();
            reuben_native::diagnostics::log_snapshot(&played.shutdown());
        });
    }
}

//! Integration: where an open resolves the opened document's relative resources.
//!
//! The opened document is its own anchor — sibling-first from its directory, then the library
//! root, exactly as a fresh start on that path. Read two ways: the reports, through the real entry
//! point with no device ([`start_headless`]); and the sound, through a render slot this file drives
//! itself, because a report can only say a voice resolved, not *which* file it was built from.
//!
//! Every fixture is a scratch copy of the library: nothing here writes under `instruments/`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use reuben_api::authoring::{validate_instrument, Report, Resources, ValidateInstrument};
use reuben_api::engine::{
    install_initial, Channel, InProcess, StructureState, SwapOutcome, SwapReport,
};
use reuben_api::render::{AudioConfig, RenderSlot};
use reuben_api::staged::Staged;
use reuben_api::FsResolver;
use reuben_native::control::{ring_pair, ControlRing};
use reuben_native::diagnostics::Diagnostics;
use reuben_native::engine::{EngineConfig, Instrument};
use reuben_native::structure::NativeHost;
use reuben_native::test_support::{served, start_headless, within};

/// A voice that sounds with no note held: an oscillator straight to `audio`, and no `active`
/// output, so the voicer can never judge it idle and skip it. The library's `default-voice` is
/// silent until gated, which makes the two tell apart in one rendered block.
const DRONE_VOICE: &str = r#"{
    "format_version": 2,
    "instrument": "drone-voice",
    "interface": {
        "inputs": {
            "freq": { "type": "f32_buffer", "default": 440.0, "min": 20.0, "max": 20000.0 },
            "gate": { "type": "f32", "default": 0.0, "min": 0.0, "max": 1.0 }
        },
        "outputs": { "audio": { "from": "/osc.audio" } }
    },
    "nodes": [
        { "type": "oscillator", "address": "/osc", "inputs": { "freq": { "from": "/freq" } } }
    ]
}"#;

const HANG_SECS: u64 = 30;

/// A scratch directory removed on drop, so a failed assertion leaves nothing behind.
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("reuben_open_anchor_{test}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the scratch directory");
        Scratch(dir)
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    /// Copy the whole library to `lib/` and return that directory.
    fn library(&self) -> PathBuf {
        let lib = self.path("lib");
        copy_dir(&library_dir(), &lib);
        lib
    }

    /// A copy of the library's `default.json` at `<dir>/default.json` whose one voice resource
    /// names `voices/<voice>`, with [`DRONE_VOICE`] written there.
    fn drone_song(&self, dir: &str, voice: &str) -> PathBuf {
        let song = self.path(dir).join("default.json");
        let text = std::fs::read_to_string(library_dir().join("default.json"))
            .expect("read the library's default instrument")
            .replace("voices/default-voice.json", &format!("voices/{voice}"));
        write(&song, &text);
        write(&self.path(dir).join("voices").join(voice), DRONE_VOICE);
        song
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn library_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../instruments")
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("a file path has a parent"))
        .expect("create the parent directory");
    std::fs::write(path, text).expect("write a fixture");
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

fn spelled(path: &Path) -> String {
    path.display().to_string()
}

fn installed(outcome: SwapOutcome) -> SwapReport {
    match outcome {
        SwapOutcome::Installed(report) => report,
        SwapOutcome::Conflict(conflict) => panic!("an unguarded open conflicted: {conflict:?}"),
        SwapOutcome::Held(held) => panic!("reuben play held an open: {held:?}"),
        SwapOutcome::Refused(message) => panic!("reuben play refused an open: {message}"),
    }
}

fn open_path(channel: &Channel, path: &Path) -> SwapReport {
    installed(
        channel
            .open(spelled(path), None)
            .expect("the open reaches the engine"),
    )
}

/// `validate_instrument` on `path`, the way the CLI and the MCP sidecar open a document's store.
fn validate(path: &Path, root: Option<&Path>) -> Report {
    let source = spelled(path);
    let store = FsResolver::for_document(&source);
    let store = match root {
        Some(root) => store.with_root(root),
        None => store,
    };
    validate_instrument(&ValidateInstrument { source }, &store)
        .expect("the document reads")
        .output
}

fn headless(launch: &Path, root: Option<&Path>) -> EngineConfig {
    EngineConfig {
        instrument_root: root.map(Path::to_path_buf),
        osc_in: None,
        structure: None,
        // A followed write would install before the open these tests make, so its diff would
        // read against the edit rather than what played.
        follow_files: false,
        ..EngineConfig::new(Instrument::Path(launch.to_path_buf()))
    }
}

fn assert_clean(report: &SwapReport, what: &str) {
    assert!(report.report.ok, "{what}: {:?}", report.report.errors);
    assert!(
        report.report.warnings.is_empty(),
        "{what}: {:?}",
        report.report.warnings
    );
}

#[test]
fn an_opened_documents_voice_is_fingerprinted_from_its_own_directory() {
    within(HANG_SECS, || {
        let scratch = Scratch::new("fingerprint");
        let lib = scratch.library();
        let song = scratch.drone_song("song", "default-voice.json");

        let engine = start_headless(headless(&lib.join("euclidean-drums.json"), None))
            .expect("start on a library instrument");
        engine.offer_dir(&scratch.0);
        let channel = engine.channel();

        assert_clean(&open_path(&channel, &song), "the copy");

        // The library's `default.json` is the same document as the copy, byte for byte: the only
        // thing that can tell their voicers apart is which `voices/default-voice.json` each read.
        let back = open_path(&channel, &lib.join("default.json"));
        assert_clean(&back, "the library's own");
        let diff = back.diff.expect("an installed open carries a diff");
        assert_eq!(
            diff.state_reset,
            ["/voicer"],
            "the copy's voicer hosted the copy's voice, so the library's is a different node"
        );
        engine.shutdown();
    });
}

#[test]
fn a_voice_only_the_opened_documents_directory_holds_is_found() {
    within(HANG_SECS, || {
        let scratch = Scratch::new("only_here");
        let lib = scratch.library();
        let song = scratch.drone_song("song", "song-only-voice.json");

        let engine = start_headless(headless(&lib.join("euclidean-drums.json"), None))
            .expect("start on a library instrument");
        engine.offer_dir(&scratch.0);
        assert_clean(&open_path(&engine.channel(), &song), "the copy");
        engine.shutdown();
    });
}

/// A relative name is read against the working directory, so its resources resolve from there too
/// and not from the launch instrument's directory. Cargo runs this binary in the crate's own
/// directory, which is what makes the spelling below name the library.
#[test]
fn a_relative_name_is_anchored_at_the_working_directory() {
    within(HANG_SECS, || {
        let scratch = Scratch::new("relative");
        let song = scratch.drone_song("song", "default-voice.json");

        let engine = start_headless(headless(&song, None)).expect("start on the scratch song");
        engine.offer_dir(&library_dir());
        let report = open_path(
            &engine.channel(),
            Path::new("../../instruments/default.json"),
        );
        assert_clean(&report, "the library's default, spelled relatively");
        engine.shutdown();
    });
}

#[test]
fn an_open_report_agrees_with_validate_instrument() {
    within(HANG_SECS, || {
        let scratch = Scratch::new("agrees");
        let lib = scratch.library();
        let found = scratch.drone_song("song", "song-only-voice.json");
        // The voice this one names is beside the launch instrument and not beside the document.
        let missing = scratch.path("bare/default.json");
        std::fs::create_dir_all(missing.parent().unwrap()).unwrap();
        std::fs::copy(lib.join("default.json"), &missing).unwrap();

        let engine = start_headless(headless(&lib.join("euclidean-drums.json"), None))
            .expect("start on a library instrument");
        engine.offer_dir(&scratch.0);
        let channel = engine.channel();

        for path in [&found, &missing] {
            let opened = open_path(&channel, path).report;
            assert_eq!(opened, validate(path, None), "{}", path.display());
        }
        assert!(
            !validate(&missing, None).warnings.is_empty(),
            "the bare copy has no voice to find, so the agreement above was about a warning"
        );
        engine.shutdown();
    });
}

#[test]
fn opening_the_launch_file_keeps_every_survivor() {
    within(HANG_SECS, || {
        let scratch = Scratch::new("launch_file");
        let lib = scratch.library();
        let launch = lib.join("euclidean-drums.json");

        let engine = start_headless(headless(&launch, None)).expect("start");
        let channel = engine.channel();

        // Three spellings of the launch file. Each reads as exactly what plays, the documents it
        // reaches included, so every node survives with its state: a spelling that resolved a
        // voice from elsewhere would reset the node hosting it.
        let spellings = [
            launch.clone(),
            lib.join(".").join("euclidean-drums.json"),
            lib.join("voices").join("..").join("euclidean-drums.json"),
        ];
        for path in &spellings {
            let report = open_path(&channel, path);
            assert_clean(&report, "the launch file");
            let diff = report.diff.expect("an installed open carries a diff");
            assert!(
                diff.state_reset.is_empty() && diff.added.is_empty() && diff.removed.is_empty(),
                "{}: {diff:?}",
                path.display()
            );
        }

        // An edit to a runtime input, then the open an author makes after it.
        let text = std::fs::read_to_string(&launch).unwrap();
        let mut doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        doc["interface"]["inputs"]["tempo"]["default"] = serde_json::json!(97.0);
        std::fs::write(&launch, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        let report = open_path(&channel, &launch);
        assert_clean(&report, "the edited launch file");
        let diff = report.diff.expect("an installed open carries a diff");
        assert!(
            diff.added.is_empty() && diff.removed.is_empty(),
            "an edit to a pipe default moves no node: {diff:?}"
        );
        assert!(
            diff.state_reset.is_empty(),
            "the voices beside the launch file are the same files as before: {diff:?}"
        );
        engine.shutdown();
    });
}

#[test]
fn a_sibling_of_the_launch_file_resolves_as_the_launch_file_does() {
    within(HANG_SECS, || {
        let scratch = Scratch::new("sibling");
        let lib = scratch.library();

        let engine = start_headless(headless(&lib.join("default.json"), None)).expect("start");
        let channel = engine.channel();
        let sibling = lib.join("euclidean-drums.json");
        assert_clean(&open_path(&channel, &sibling), "a sibling");

        // Opened again, the sibling resolves its resources as it did the first time: one identity
        // per resource, so every node survives.
        let report = open_path(&channel, &sibling);
        assert_clean(&report, "the sibling again");
        let diff = report.diff.expect("an installed open carries a diff");
        assert!(
            diff.state_reset.is_empty() && diff.added.is_empty() && diff.removed.is_empty(),
            "{diff:?}"
        );
        engine.shutdown();
    });
}

#[test]
fn the_library_root_is_the_fallback_after_the_opened_documents_directory() {
    within(HANG_SECS, || {
        let scratch = Scratch::new("root");
        let lib = scratch.library();
        // The launch instrument is outside the library root, so a hit on the root is not also a
        // hit on the session's anchor.
        let launch = scratch.drone_song("start", "default-voice.json");
        let bare = scratch.path("bare/default.json");
        std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
        std::fs::copy(lib.join("default.json"), &bare).unwrap();

        let engine = start_headless(headless(&launch, Some(&lib))).expect("start");
        engine.offer_dir(&scratch.0);
        let report = open_path(&engine.channel(), &bare);
        assert_clean(&report, "a document with nothing beside it");
        assert_eq!(report.report, validate(&bare, Some(&lib)));
        let diff = report.diff.expect("an installed open carries a diff");
        assert_eq!(
            diff.state_reset,
            ["/voicer"],
            "the root's voice, not the launch directory's drone of the same name"
        );
        engine.shutdown();
    });
}

/// The block this file renders at.
const BLOCK: usize = 128;

/// An engine whose render slot the test drives, served by the same host and window `reuben play`
/// wires up, so what an open installed can be listened to.
struct Played {
    channel: Channel,
    slot: RenderSlot,
    /// The control ingress the host sends into; held so a `send` would not find it closed.
    _control: ControlRing,
}

impl Played {
    /// The anchor is the one `reuben play <launch> [--instrument-root <root>]` builds. Everything
    /// under `offered` may be opened.
    fn start(launch: &Path, root: Option<&Path>, offered: &Path) -> Self {
        let text = std::fs::read_to_string(launch).expect("read the launch instrument");
        let store = FsResolver::for_instrument(launch);
        let store = match root {
            Some(root) => store.with_root(root),
            None => store,
        };
        let staged: Staged<Box<dyn Resources + Send + Sync>> = Staged::new(Box::new(store));
        let (coordinator, side, warnings) =
            install_initial(&text, staged.clone(), AudioConfig::new(48_000.0, BLOCK))
                .expect("install");
        assert!(warnings.is_empty(), "{warnings:?}");
        let (control, control_rx) = ring_pair(Diagnostics::new());
        let host = NativeHost::new(Diagnostics::new(), control);
        host.offer_dir(offered);
        let state = StructureState::new(coordinator, Arc::new(host), served(staged))
            .with_installed_source(Some(spelled(launch)));
        Played {
            channel: Channel::new(InProcess::new(state)),
            slot: RenderSlot::new(side),
            _control: control_rx,
        }
    }

    fn render(&mut self, blocks: usize) -> f32 {
        let mut peak = 0.0f32;
        let mut buf = Vec::new();
        for _ in 0..blocks {
            buf.resize(BLOCK * self.slot.channels().max(1), 0.0);
            self.slot.fill(&mut buf);
            peak = buf.iter().fold(peak, |m, &s| m.max(s.abs()));
        }
        peak
    }

    /// The peak of what is installed, once a swap's crossfade has run out.
    fn settled_peak(&mut self) -> f32 {
        // One second: far past any declick ramp, so what follows is the new Engine alone.
        self.render(48_000 / BLOCK);
        self.render(64)
    }
}

#[test]
fn what_an_open_installs_sounds_like_the_voice_beside_the_opened_document() {
    let scratch = Scratch::new("sounds");
    let lib = scratch.library();
    let song = scratch.drone_song("song", "default-voice.json");

    // With the root set too: the opened document's own directory still comes first.
    for root in [None, Some(lib.as_path())] {
        let mut played = Played::start(&lib.join("default.json"), root, &scratch.0);
        assert_eq!(
            played.settled_peak(),
            0.0,
            "the library's voice is silent until a note gates it"
        );

        assert_clean(&open_path(&played.channel, &song), "the copy");
        assert!(
            played.settled_peak() > 0.01,
            "the copy's voice drones; silence here is the library's voice of the same name"
        );
    }
}

#[test]
fn opening_back_resolves_from_the_first_documents_directory_again() {
    let scratch = Scratch::new("back");
    let lib = scratch.library();
    let song = scratch.drone_song("song", "default-voice.json");
    let launch = lib.join("default.json");

    let mut played = Played::start(&launch, None, &scratch.0);
    assert_clean(&open_path(&played.channel, &song), "the copy");
    assert!(played.settled_peak() > 0.01, "the copy's drone");

    assert_clean(&open_path(&played.channel, &launch), "the launch file");
    assert_eq!(
        played.settled_peak(),
        0.0,
        "back on the library's silent voice, not the copy's drone"
    );

    // And a document whose voices only the first directory holds still finds them there.
    let drums = open_path(&played.channel, &lib.join("euclidean-drums.json"));
    assert_clean(&drums, "a sibling of the launch file");
    assert!(played.settled_peak() > 0.0, "the drum machine plays itself");
}

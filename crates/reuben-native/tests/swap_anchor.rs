//! Integration: where a by-path Swap resolves the swapped document's relative resources.
//!
//! The swapped document is its own anchor — sibling-first from its directory, then the library
//! root, exactly as a fresh start on that path — while a by-value Swap, which has no source, keeps
//! the session's. Read two ways: the reports, through the real entry point with no device
//! ([`start_headless`]); and the sound, through a render slot this file drives itself, because a
//! report can only say a voice resolved, not *which* file it was built from.
//!
//! Every fixture is a scratch copy of the library: nothing here writes under `instruments/`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

use reuben_api::authoring::{validate_instrument, Report, ValidateInstrument};
use reuben_api::engine::{
    install_initial, Channel, DocSource, InProcess, StructureState, SwapOutcome, SwapReport,
};
use reuben_api::render::{AudioConfig, RenderSlot};
use reuben_api::FsResolver;
use reuben_native::diagnostics::Diagnostics;
use reuben_native::engine::{EngineConfig, Instrument};
use reuben_native::osc::ControlBatch;
use reuben_native::structure::NativeHost;
use reuben_native::test_support::{start_headless, within};

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
            std::env::temp_dir().join(format!("reuben_swap_anchor_{test}_{}", std::process::id()));
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
        SwapOutcome::Conflict(conflict) => panic!("an unguarded swap conflicted: {conflict:?}"),
        SwapOutcome::Held(held) => panic!("reuben play held a swap: {held:?}"),
    }
}

fn swap_path(channel: &Channel, path: &Path) -> SwapReport {
    installed(
        channel
            .swap(DocSource::Path(spelled(path)), None)
            .expect("the swap reaches the engine"),
    )
}

fn swap_value(channel: &Channel, path: &Path) -> SwapReport {
    let text = std::fs::read_to_string(path).expect("read the document to send by value");
    let value = serde_json::from_str(&text).expect("the document is JSON");
    installed(
        channel
            .swap(DocSource::Document(value), None)
            .expect("the swap reaches the engine"),
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
fn a_swapped_documents_voice_is_fingerprinted_from_its_own_directory() {
    within(HANG_SECS, || {
        let scratch = Scratch::new("fingerprint");
        let lib = scratch.library();
        let song = scratch.drone_song("song", "default-voice.json");

        let engine = start_headless(headless(&lib.join("euclidean-drums.json"), None))
            .expect("start on a library instrument");
        let channel = engine.channel();

        assert_clean(&swap_path(&channel, &song), "the copy");

        // The library's `default.json` is the same document as the copy, byte for byte: the only
        // thing that can tell their voicers apart is which `voices/default-voice.json` each read.
        let back = swap_path(&channel, &lib.join("default.json"));
        assert_clean(&back, "the library's own");
        let diff = back.diff.expect("an installed swap carries a diff");
        assert_eq!(
            diff.state_reset,
            ["/voicer"],
            "the copy's voicer hosted the copy's voice, so the library's is a different node"
        );
        engine.shutdown();
    });
}

#[test]
fn a_voice_only_the_swapped_documents_directory_holds_is_found() {
    within(HANG_SECS, || {
        let scratch = Scratch::new("only_here");
        let lib = scratch.library();
        let song = scratch.drone_song("song", "song-only-voice.json");

        let engine = start_headless(headless(&lib.join("euclidean-drums.json"), None))
            .expect("start on a library instrument");
        assert_clean(&swap_path(&engine.channel(), &song), "the copy");
        engine.shutdown();
    });
}

/// A relative swap path is read against the working directory, so its resources resolve from
/// there too and not from the launch instrument's directory. Cargo runs this binary in the crate's
/// own directory, which is what makes the spelling below name the library.
#[test]
fn a_relative_swap_path_is_anchored_at_the_working_directory() {
    within(HANG_SECS, || {
        let scratch = Scratch::new("relative");
        let song = scratch.drone_song("song", "default-voice.json");

        let engine = start_headless(headless(&song, None)).expect("start on the scratch song");
        let report = swap_path(
            &engine.channel(),
            Path::new("../../instruments/default.json"),
        );
        assert_clean(&report, "the library's default, spelled relatively");
        engine.shutdown();
    });
}

#[test]
fn a_swap_report_agrees_with_validate_instrument() {
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
        let channel = engine.channel();

        for path in [&found, &missing] {
            let swapped = swap_path(&channel, path).report;
            assert_eq!(swapped, validate(path, None), "{}", path.display());
        }
        assert!(
            !validate(&missing, None).warnings.is_empty(),
            "the bare copy has no voice to find, so the agreement above was about a warning"
        );
        engine.shutdown();
    });
}

#[test]
fn swapping_the_launch_file_keeps_every_survivor() {
    within(HANG_SECS, || {
        let scratch = Scratch::new("launch_file");
        let lib = scratch.library();
        let launch = lib.join("euclidean-drums.json");

        let engine = start_headless(headless(&launch, None)).expect("start");
        let channel = engine.channel();

        // Three spellings of the launch file, none of them the one the engine started on.
        let spellings = [
            launch.clone(),
            lib.join(".").join("euclidean-drums.json"),
            lib.join("voices").join("..").join("euclidean-drums.json"),
        ];
        let mut nodes = None;
        for path in &spellings {
            let report = swap_path(&channel, path);
            assert_clean(&report, "the launch file");
            let diff = report.diff.expect("an installed swap carries a diff");
            assert!(
                diff.state_reset.is_empty() && diff.added.is_empty() && diff.removed.is_empty(),
                "{}: {diff:?}",
                path.display()
            );
            assert!(diff.survived > 0);
            assert_eq!(*nodes.get_or_insert(diff.survived), diff.survived);
        }

        // An edit to a runtime input, then the Swap an author makes after it.
        let text = std::fs::read_to_string(&launch).unwrap();
        let mut doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        doc["interface"]["inputs"]["tempo"]["default"] = serde_json::json!(97.0);
        std::fs::write(&launch, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        let report = swap_path(&channel, &launch);
        assert_clean(&report, "the edited launch file");
        let diff = report.diff.expect("an installed swap carries a diff");
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
        assert_clean(&swap_path(&channel, &sibling), "a sibling by path");

        // By value the same document resolves from the session's anchor, which is the directory
        // the sibling is in: one identity per resource either way, so every node survives.
        let report = swap_value(&channel, &sibling);
        assert_clean(&report, "the sibling by value");
        let diff = report.diff.expect("an installed swap carries a diff");
        assert!(
            diff.state_reset.is_empty() && diff.added.is_empty() && diff.removed.is_empty(),
            "{diff:?}"
        );
        engine.shutdown();
    });
}

#[test]
fn a_swap_by_value_keeps_the_sessions_anchor() {
    within(HANG_SECS, || {
        let scratch = Scratch::new("by_value");
        let lib = scratch.library();
        let song = scratch.drone_song("song", "song-only-voice.json");

        let engine = start_headless(headless(&lib.join("euclidean-drums.json"), None))
            .expect("start on a library instrument");
        let channel = engine.channel();

        let from_session = swap_value(&channel, &lib.join("default.json"));
        assert_clean(
            &from_session,
            "a document whose voice is beside the launch file",
        );

        // A by-path Swap in between must not leave its anchor behind for the next by-value one.
        assert_clean(&swap_path(&channel, &song), "the copy by path");
        let report = swap_value(&channel, &song);
        assert!(report.report.ok, "{:?}", report.report.errors);
        assert!(
            report
                .report
                .warnings
                .iter()
                .any(|w| w.message.contains("song-only-voice.json")),
            "by value there is no directory to find the copy's voice in: {:?}",
            report.report.warnings
        );
        assert_clean(
            &swap_value(&channel, &lib.join("default.json")),
            "the session's anchor again",
        );
        engine.shutdown();
    });
}

#[test]
fn the_library_root_is_the_fallback_after_the_swapped_documents_directory() {
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
        let report = swap_path(&engine.channel(), &bare);
        assert_clean(&report, "a document with nothing beside it");
        assert_eq!(report.report, validate(&bare, Some(&lib)));
        let diff = report.diff.expect("an installed swap carries a diff");
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
/// wires up, so what a Swap installed can be listened to.
struct Played {
    channel: Channel,
    slot: RenderSlot,
    /// The control ingress the host sends into; held so a `send` would not find it closed.
    _control: Receiver<ControlBatch>,
}

impl Played {
    /// The anchor is the one `reuben play <launch> [--instrument-root <root>]` builds.
    fn start(launch: &Path, root: Option<&Path>) -> Self {
        let text = std::fs::read_to_string(launch).expect("read the launch instrument");
        let store = FsResolver::for_instrument(launch);
        let store = match root {
            Some(root) => store.with_root(root),
            None => store,
        };
        let (coordinator, side, warnings) =
            install_initial(&text, store, AudioConfig::new(48_000.0, BLOCK)).expect("install");
        assert!(warnings.is_empty(), "{warnings:?}");
        let (control, control_rx) = mpsc::channel::<ControlBatch>();
        let host = NativeHost::new(Diagnostics::new(), control);
        let state = StructureState::new(coordinator, Arc::new(host));
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

    /// The peak of what is installed, once a Swap's crossfade has run out.
    fn settled_peak(&mut self) -> f32 {
        // One second: far past any declick ramp, so what follows is the new Engine alone.
        self.render(48_000 / BLOCK);
        self.render(64)
    }
}

#[test]
fn what_a_swap_installs_sounds_like_the_voice_beside_the_swapped_document() {
    let scratch = Scratch::new("sounds");
    let lib = scratch.library();
    let song = scratch.drone_song("song", "default-voice.json");

    // With the root set too: the swapped document's own directory still comes first.
    for root in [None, Some(lib.as_path())] {
        let mut played = Played::start(&lib.join("default.json"), root);
        assert_eq!(
            played.settled_peak(),
            0.0,
            "the library's voice is silent until a note gates it"
        );

        assert_clean(&swap_path(&played.channel, &song), "the copy");
        assert!(
            played.settled_peak() > 0.01,
            "the copy's voice drones; silence here is the library's voice of the same name"
        );
    }
}

#[test]
fn swapping_back_resolves_from_the_first_documents_directory_again() {
    let scratch = Scratch::new("back");
    let lib = scratch.library();
    let song = scratch.drone_song("song", "default-voice.json");
    let launch = lib.join("default.json");

    let mut played = Played::start(&launch, None);
    assert_clean(&swap_path(&played.channel, &song), "the copy");
    assert!(played.settled_peak() > 0.01, "the copy's drone");

    assert_clean(&swap_path(&played.channel, &launch), "the launch file");
    assert_eq!(
        played.settled_peak(),
        0.0,
        "back on the library's silent voice, not the copy's drone"
    );

    // And a document whose voices only the first directory holds still finds them there.
    let drums = swap_path(&played.channel, &lib.join("euclidean-drums.json"));
    assert_clean(&drums, "a sibling of the launch file");
    assert!(played.settled_peak() > 0.0, "the drum machine plays itself");
}

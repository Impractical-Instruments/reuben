//! MIDI routing in `play`: translated messages injected with `send_midi` are routed by the open
//! instrument's MIDI map (`surfaces/<stem>.midi.json`), which `play` rebuilds on every swap and
//! map edit.

use std::path::Path;
use std::time::{Duration, Instant};

mod common;

use common::{reuben, spawn_play, TempDir};

use reuben_api::engine::{
    CallOutcome, Channel, ControlArg, DiagnosticsReport, SwapOutcome, TcpTransport,
    DEFAULT_CONNECT_TIMEOUT,
};
use reuben_native::engine::{start_headless, EngineConfig, Instrument, RunningEngine, StartError};

/// One exponential `f32` input and one `note` input, both feeding a node.
fn instrument(name: &str) -> String {
    format!(
        r#"{{"format_version":3,"instrument":"{name}",
        "interface":{{
            "inputs":{{
                "cutoff":{{"type":"f32","default":1000,"min":20,"max":20000,"curve":"exp"}},
                "notes":{{"type":"note"}}
            }},
            "outputs":{{"out":{{"from":"/osc.audio"}}}}
        }},
        "nodes":[
            {{"type":"oscillator","address":"/osc","inputs":{{"freq":{{"from":"/cutoff"}}}}}},
            {{"type":"unpack_note","address":"/unpack","inputs":{{"in":{{"from":"/notes"}}}}}}
        ]}}"#
    )
}

/// A MIDI map for `name` binding `cutoff` to CC 74 on `channel` and `notes` to channel 1.
fn map(name: &str, channel: u8) -> String {
    format!(
        r#"{{"surface_version":1,"instrument":"{name}","controls":[
            {{"bind":"cutoff","midi":"/midi/{channel}/cc/74","min":200,"max":8000}},
            {{"bind":"notes","midi":"/midi/1/note"}}
        ]}}"#
    )
}

fn seed(dir: &Path, name: &str, map_channel: Option<u8>) {
    std::fs::write(dir.join(format!("{name}.json")), instrument(name)).expect("seed");
    if let Some(channel) = map_channel {
        write_map(dir, name, &map(name, channel));
    }
}

fn write_map(dir: &Path, name: &str, text: &str) {
    std::fs::create_dir_all(dir.join("surfaces")).expect("surfaces dir");
    std::fs::write(dir.join("surfaces").join(format!("{name}.midi.json")), text).expect("map");
}

fn start(dir: &Path, name: &str) -> RunningEngine {
    start_headless(EngineConfig {
        osc_in: None,
        engine_addr: None,
        ..EngineConfig::new(Instrument::Path(dir.join(format!("{name}.json"))))
    })
    .expect("start headless")
}

fn diagnostics(channel: &Channel) -> DiagnosticsReport {
    channel.get_diagnostics().expect("diagnostics")
}

fn cc(channel: &Channel, midi_channel: u8, value: f32) {
    channel
        .send_midi(
            format!("/midi/{midi_channel}/cc/74"),
            vec![ControlArg::F32(value)],
        )
        .expect("send_midi");
}

/// Whether CC 74 on `midi_channel` routes within `within`: sent until `midi_routed` moves, since
/// the route table is rebuilt off the caller's thread.
fn routes(channel: &Channel, midi_channel: u8, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    loop {
        let before = diagnostics(channel).midi_routed;
        cc(channel, midi_channel, 0.5);
        if diagnostics(channel).midi_routed > before {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

const SETTLE: Duration = Duration::from_secs(5);
/// Longer than several rebuild polls, so a rebuild that was going to happen has.
const QUIET: Duration = Duration::from_millis(800);

#[test]
fn injected_cc_is_routed() {
    let dir = TempDir::new("midi_injected_cc");
    seed(&dir.0, "inst", Some(2));
    let engine = start(&dir.0, "inst");
    let channel = engine.channel();
    assert!(routes(&channel, 2, SETTLE), "{:?}", diagnostics(&channel));
}

#[test]
fn unmatched_injection_counts_unmatched() {
    let dir = TempDir::new("midi_unmatched");
    seed(&dir.0, "inst", Some(2));
    let engine = start(&dir.0, "inst");
    let channel = engine.channel();
    assert!(routes(&channel, 2, SETTLE));
    let before = diagnostics(&channel);
    cc(&channel, 9, 0.5);
    let after = diagnostics(&channel);
    assert_eq!(after.midi_unmatched, before.midi_unmatched + 1);
    assert_eq!(after.midi_routed, before.midi_routed);
}

#[test]
fn editing_the_map_reroutes() {
    let dir = TempDir::new("midi_edit_map");
    seed(&dir.0, "inst", Some(2));
    let engine = start(&dir.0, "inst");
    let channel = engine.channel();
    assert!(routes(&channel, 2, SETTLE));
    write_map(&dir.0, "inst", &map("inst", 3));
    assert!(routes(&channel, 3, SETTLE), "{:?}", diagnostics(&channel));
    assert!(!routes(&channel, 2, Duration::ZERO));
}

#[test]
fn invalid_map_keeps_previous_routes() {
    let dir = TempDir::new("midi_invalid_map");
    seed(&dir.0, "inst", Some(2));
    let engine = start(&dir.0, "inst");
    let channel = engine.channel();
    assert!(routes(&channel, 2, SETTLE));
    write_map(&dir.0, "inst", "{ not json");
    std::thread::sleep(QUIET);
    assert!(routes(&channel, 2, Duration::ZERO));
}

#[test]
fn a_map_edit_while_the_file_does_not_load_keeps_the_routes_playing() {
    let dir = TempDir::new("midi_unloadable_file");
    seed(&dir.0, "inst", Some(2));
    let engine = start(&dir.0, "inst");
    let channel = engine.channel();
    assert!(routes(&channel, 2, SETTLE));
    // The previous instrument keeps playing; the file no longer loads.
    std::fs::write(dir.0.join("inst.json"), "{ not an instrument").expect("break it");
    std::thread::sleep(QUIET);
    write_map(&dir.0, "inst", &map("inst", 3));
    std::thread::sleep(QUIET);
    assert!(
        routes(&channel, 2, Duration::ZERO),
        "{:?}",
        diagnostics(&channel)
    );
}

#[test]
fn a_deleted_map_routes_nothing() {
    let dir = TempDir::new("midi_deleted_map");
    seed(&dir.0, "inst", Some(2));
    let engine = start(&dir.0, "inst");
    let channel = engine.channel();
    assert!(routes(&channel, 2, SETTLE));
    std::fs::remove_file(dir.0.join("surfaces/inst.midi.json")).expect("remove the map");
    let deadline = Instant::now() + SETTLE;
    while routes(&channel, 2, Duration::ZERO) {
        assert!(Instant::now() < deadline, "the deleted map still routes");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn no_map_routes_nothing() {
    let dir = TempDir::new("midi_no_map");
    seed(&dir.0, "inst", None);
    let engine = start(&dir.0, "inst");
    let channel = engine.channel();
    std::thread::sleep(QUIET);
    cc(&channel, 2, 0.5);
    channel
        .send_midi(
            "/midi/1/note".to_string(),
            vec![ControlArg::I32(60), ControlArg::F32(1.0)],
        )
        .expect("send_midi");
    let report = diagnostics(&channel);
    assert_eq!((report.midi_routed, report.midi_unmatched), (0, 2));
}

#[test]
fn opening_another_instrument_follows_its_map() {
    let dir = TempDir::new("midi_open_follows");
    seed(&dir.0, "first", Some(2));
    seed(&dir.0, "second", Some(5));
    let engine = start(&dir.0, "first");
    let channel = engine.channel();
    assert!(routes(&channel, 2, SETTLE));
    let opened = channel
        .open(dir.0.join("second.json").display().to_string(), None)
        .expect("open");
    assert!(matches!(opened, SwapOutcome::Installed(_)), "{opened:?}");
    assert!(routes(&channel, 5, SETTLE), "{:?}", diagnostics(&channel));
    assert!(!routes(&channel, 2, Duration::ZERO));
}

#[test]
fn opening_an_instrument_whose_map_is_broken_drops_the_old_routes() {
    let dir = TempDir::new("midi_open_broken_map");
    seed(&dir.0, "first", Some(2));
    seed(&dir.0, "second", None);
    write_map(&dir.0, "second", "{ not json");
    let engine = start(&dir.0, "first");
    let channel = engine.channel();
    assert!(routes(&channel, 2, SETTLE));
    channel
        .open(dir.0.join("second.json").display().to_string(), None)
        .expect("open");
    let deadline = Instant::now() + SETTLE;
    while routes(&channel, 2, Duration::ZERO) {
        assert!(
            Instant::now() < deadline,
            "the first instrument's map still routes"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn send_midi_from_the_cli_reaches_the_map() {
    let dir = TempDir::new("midi_cli");
    seed(&dir.0, "inst", Some(2));
    let play = spawn_play(&dir.0, &["inst.json"]);
    let channel = Channel::new(TcpTransport::new(
        play.addr.clone(),
        DEFAULT_CONNECT_TIMEOUT,
    ));
    let send = |address: &str| {
        reuben(
            &dir.0,
            &["send-midi", "--engine-addr", &play.addr, address, "0.5"],
        )
    };
    let deadline = Instant::now() + SETTLE;
    while diagnostics(&channel).midi_routed == 0 {
        let out = send("/midi/2/cc/74");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(Instant::now() < deadline, "{:?}", diagnostics(&channel));
        std::thread::sleep(Duration::from_millis(50));
    }
    let refused = send("/midi/17/cc/74");
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("/midi/17/cc/74"), "{stderr}");
}

#[test]
fn a_map_edit_routes_with_file_following_off() {
    let dir = TempDir::new("midi_verb_edit");
    seed(&dir.0, "inst", Some(2));
    let engine = start_headless(EngineConfig {
        osc_in: None,
        engine_addr: None,
        follow_files: false,
        ..EngineConfig::new(Instrument::Path(dir.0.join("inst.json")))
    })
    .expect("start headless");
    let channel = engine.channel();
    assert!(routes(&channel, 2, SETTLE));
    let set = channel
        .call(
            "set_instrument_surface_control",
            serde_json::json!({
                "source": dir.0.join("inst.json").display().to_string(),
                "surface": "midi",
                "index": 0,
                "control": {"bind": "cutoff", "midi": "/midi/3/cc/74"},
            }),
        )
        .expect("set");
    let CallOutcome::Called { output, .. } = set else {
        panic!("{set:?}");
    };
    assert_eq!(output["written"], true, "{output}");
    assert!(routes(&channel, 3, SETTLE), "{:?}", diagnostics(&channel));
    assert!(!routes(&channel, 2, Duration::ZERO));
}

#[test]
fn a_midi_input_matching_no_port_stops_the_start() {
    let dir = TempDir::new("midi_no_port");
    seed(&dir.0, "inst", Some(2));
    let mut config = EngineConfig {
        osc_in: None,
        engine_addr: None,
        ..EngineConfig::new(Instrument::Path(dir.0.join("inst.json")))
    };
    config.profile.midi.inputs = vec!["no-such-midi-port-xyz".to_string()];
    let Err(error) = start_headless(config) else {
        panic!("started with a MIDI input that matches no port");
    };
    assert!(matches!(error, StartError::MidiDevice(_)), "{error:?}");
    assert!(
        error.to_string().contains("no-such-midi-port-xyz"),
        "{error}"
    );
}

/// A virtual port stands in for a keyboard: `play` opens it by name, its notes route through the
/// map, and closing it is counted as a lost device. Needs an ALSA sequencer.
#[cfg(target_os = "linux")]
#[test]
fn a_device_port_routes_and_its_loss_is_counted() {
    use midir::os::unix::VirtualOutput;
    let Ok(output) = midir::MidiOutput::new("reuben test keyboard") else {
        eprintln!("skipped: no ALSA sequencer to make a virtual MIDI port on");
        return;
    };
    let name = format!("reuben-test-kbd-{}", std::process::id());
    let mut keyboard = output.create_virtual(&name).expect("virtual port");
    let dir = TempDir::new("midi_device_port");
    seed(&dir.0, "inst", Some(2));
    let mut config = EngineConfig {
        osc_in: None,
        engine_addr: None,
        ..EngineConfig::new(Instrument::Path(dir.0.join("inst.json")))
    };
    config.profile.midi.inputs = vec![name.to_uppercase()];
    let engine = start_headless(config).expect("start with the virtual port");
    let channel = engine.channel();
    // The map's routes install off the start thread: CC 74 routing means the note route has too.
    assert!(routes(&channel, 2, SETTLE));
    let before = diagnostics(&channel);
    keyboard.send(&[0x90, 60, 100]).expect("note on");
    let deadline = Instant::now() + SETTLE;
    while diagnostics(&channel).midi_routed == before.midi_routed {
        assert!(Instant::now() < deadline, "{:?}", diagnostics(&channel));
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(
        diagnostics(&channel).midi_received,
        before.midi_received + 1
    );
    drop(keyboard);
    let deadline = Instant::now() + SETTLE;
    while diagnostics(&channel).midi_devices_lost == 0 {
        assert!(Instant::now() < deadline, "{:?}", diagnostics(&channel));
        std::thread::sleep(Duration::from_millis(50));
    }
}

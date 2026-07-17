//! Integration: the JSON instrument format produces a working, deterministic rig.

use reuben_core::message::{Arg, Message};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::{ResolveError, ResourceResolver, SampleBuffer};
use reuben_core::vocab::pitch::{Note, Pitch};
use reuben_core::{load, load_instrument, AudioConfig, Graph, NormalizedDoc, Registry};

const DEFAULT_JSON: &str = include_str!("../../../instruments/default.json");
const SAMPLER_VOICE_JSON: &str = include_str!("fixtures/voices/sampler-voice.json");
const METRONOME_JSON: &str = include_str!("fixtures/metronome.json");
const SEQUENCE_JSON: &str = include_str!("fixtures/sequence.json");
const GOOD_BUTTON_JSON: &str = include_str!("fixtures/good-button.json");
const AUTO_FILTER_JSON: &str = include_str!("fixtures/auto-filter.json");

/// A filesystem resolver rooted at the frozen `tests/fixtures/` tree, so an instrument-resource (`voice`)
/// reference (ADR-0032) resolves to its on-disk voice patch. Samples aren't needed by the patches
/// these tests load, so `resolve` is a stub.
struct InstrumentsDir;

impl ResourceResolver for InstrumentsDir {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        let path = format!("{}/tests/fixtures/{source}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).map_err(|e| ResolveError::NotFound(format!("{path}: {e}")))
    }
}

/// Load `default.json` through the full resource pipeline (ADR-0032): the `voicer` node's `voice`
/// instrument-resource resolves + builds its sub-patches from disk.
fn load_default() -> Graph {
    load_res(DEFAULT_JSON)
}

/// Load any instrument JSON through the full resource pipeline (ADR-0032), resolving its `voice`
/// sub-patches from the repo `instruments/` dir.
fn load_res(json: &str) -> Graph {
    load_instrument(json, &Registry::builtin(), &InstrumentsDir)
        .expect("load instrument")
        .graph
}

/// Render `seconds` of `graph`, holding note A4 (MIDI 69) from frame 0.
fn render(graph: Graph, cfg: AudioConfig, seconds: f32) -> Vec<f32> {
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let blocks = (cfg.sample_rate * seconds) as usize / cfg.block_size;
    let mut buf = vec![0.0f32; cfg.block_size];
    let mut all = Vec::with_capacity(blocks * cfg.block_size);
    for b in 0..blocks {
        let msgs: Vec<Message> = if b == 0 {
            vec![Message::new(
                "/voicer/notes",
                Note::new(Pitch::Absolute(69.0), 1.0),
                0,
            )]
        } else {
            Vec::new()
        };
        r.render_block(&mut plan, &msgs, &mut buf);
        all.extend_from_slice(&buf);
    }
    all
}

/// Render `seconds` of `graph`, holding every MIDI note in `midis` from frame 0.
fn render_notes(graph: Graph, cfg: AudioConfig, seconds: f32, midis: &[f32]) -> Vec<f32> {
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let blocks = (cfg.sample_rate * seconds) as usize / cfg.block_size;
    let mut buf = vec![0.0f32; cfg.block_size];
    let mut all = Vec::with_capacity(blocks * cfg.block_size);
    for b in 0..blocks {
        let msgs: Vec<Message> = if b == 0 {
            midis
                .iter()
                .map(|&m| Message::new("/voicer/notes", Note::new(Pitch::Absolute(m), 1.0), 0))
                .collect()
        } else {
            Vec::new()
        };
        r.render_block(&mut plan, &msgs, &mut buf);
        all.extend_from_slice(&buf);
    }
    all
}

fn rms(buf: &[f32]) -> f32 {
    (buf.iter().map(|x| x * x).sum::<f32>() / buf.len() as f32).sqrt()
}

#[test]
fn default_instrument_loads_and_makes_a_440hz_tone() {
    let cfg = AudioConfig::new(48_000.0, 256);
    let out = render(load_default(), cfg, 1.0);

    let peak = out.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    assert!(
        peak > 0.05,
        "loaded rig produced near-silence (peak {peak})"
    );

    // Fundamental ~440 Hz over the steady portion (skip the 0.1 s attack).
    let skip = (cfg.sample_rate * 0.1) as usize;
    let mut crossings = 0usize;
    let mut prev = 0.0f32;
    for &s in &out[skip..] {
        if prev <= 0.0 && s > 0.0 {
            crossings += 1;
        }
        prev = s;
    }
    let expected = (440.0 * 0.9) as usize;
    assert!(
        (expected - 20..=expected + 20).contains(&crossings),
        "expected ~{expected} crossings, got {crossings}"
    );
}

#[test]
fn save_then_reload_renders_identically() {
    // load -> save (from_graph) -> reload must render bit-identical output. Uses the resource-free,
    // self-playing metronome so the reload needs no resolver: `from_graph` now round-trips a resource
    // *id* (see `from_graph_round_trips_resource_ids`), but the decoded bytes do not — a
    // resource-bearing instrument reloaded via plain `load` (no resolver) would render silent. This
    // test pins render-identity, not the id round-trip.
    let cfg = AudioConfig::new(48_000.0, 256);
    let reg = Registry::builtin();

    let g1 = load(METRONOME_JSON, &reg).expect("load metronome");
    let saved = NormalizedDoc::from_graph(&g1, "metronome", &reg);
    let g2 = saved.build(&reg).expect("rebuild from saved doc");

    let a = render(g1, cfg, 0.5);
    let b = render(g2, cfg, 0.5);
    assert_eq!(a.len(), b.len());
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "differ at sample {i}");
    }
}

#[test]
fn from_graph_round_trips_resource_ids() {
    // `from_graph` must re-emit a node's logical resource id — an ADR-0016 `sample` and an ADR-0032
    // `voice` — so save -> reload preserves the reference. The id is stashed at build, independent of
    // resource resolution (the decoded bytes still don't round-trip; only the id does, by design).
    let reg = Registry::builtin();

    // A `voice` instrument-resource ref (the voicer in default.json).
    let g = load(DEFAULT_JSON, &reg).expect("load default");
    let doc = NormalizedDoc::from_graph(&g, "default", &reg);
    let voicer = doc
        .nodes
        .iter()
        .find(|n| n.type_name == "voicer")
        .expect("voicer node");
    assert_eq!(voicer.voice.as_deref(), Some("default-voice"));

    // A `sample` resource ref (the sample player in the sampler voice patch).
    let g = load(SAMPLER_VOICE_JSON, &reg).expect("load sampler-voice");
    let doc = NormalizedDoc::from_graph(&g, "sampler-voice", &reg);
    let player = doc
        .nodes
        .iter()
        .find(|n| n.type_name == "sample")
        .expect("sample node");
    assert_eq!(player.sample.as_deref(), Some("blip"));
}

#[test]
fn plays_a_chord_polyphonically() {
    // A C-major triad: three notes sounding at once exercises per-voice fan-out and the
    // summing master tap. A single note uses one voice; the triad uses three, so it
    // carries clearly more energy, and it must stay deterministic.
    let cfg = AudioConfig::new(48_000.0, 256);
    let chord = [60.0, 64.0, 67.0];

    let single = render_notes(load_default(), cfg, 0.5, &chord[..1]);
    let triad = render_notes(load_default(), cfg, 0.5, &chord);

    // Past the attack, three voices sum to more energy than one.
    let win =
        |b: &[f32]| rms(&b[(cfg.sample_rate as usize / 5)..(cfg.sample_rate as usize / 5 + 4096)]);
    assert!(win(&triad) > 0.05, "triad near-silent");
    assert!(
        win(&triad) > win(&single) * 1.3,
        "triad ({}) should carry more energy than a single note ({})",
        win(&triad),
        win(&single)
    );

    // Determinism holds with polyphony.
    let again = render_notes(load_default(), cfg, 0.5, &chord);
    assert_eq!(triad.len(), again.len());
    for (i, (x, y)) in triad.iter().zip(&again).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "non-deterministic at sample {i}");
    }
}

/// Render `seconds` of `graph`, holding note A4 from frame 0 and sending one extra control
/// Message (e.g. a Good Button value) at frame 0 of the first block.
fn render_with_control(graph: Graph, cfg: AudioConfig, seconds: f32, control: Message) -> Vec<f32> {
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let blocks = (cfg.sample_rate * seconds) as usize / cfg.block_size;
    let mut buf = vec![0.0f32; cfg.block_size];
    let mut all = Vec::with_capacity(blocks * cfg.block_size);
    for b in 0..blocks {
        let msgs: Vec<Message> = if b == 0 {
            vec![
                Message::new("/voicer/notes", Note::new(Pitch::Absolute(57.0), 1.0), 0),
                control.clone(),
            ]
        } else {
            Vec::new()
        };
        r.render_block(&mut plan, &msgs, &mut buf);
        all.extend_from_slice(&buf);
    }
    all
}

#[test]
fn good_button_brightness_opens_the_filter() {
    // ADR-0017 Good Button: one /brightness knob fanned (identity map -> two ranged maps ->
    // two m2s converters) into the filter's Signal cutoff + resonance. Brightness 1.0 opens
    // the filter far wider than 0.0, so the held saw carries clearly more energy.
    let cfg = AudioConfig::new(48_000.0, 256);

    let dark = render_with_control(
        load_res(GOOD_BUTTON_JSON),
        cfg,
        1.0,
        Message::new("/brightness/in", Arg::F32(0.0), 0),
    );
    let bright = render_with_control(
        load_res(GOOD_BUTTON_JSON),
        cfg,
        1.0,
        Message::new("/brightness/in", Arg::F32(1.0), 0),
    );

    // Steady-state window past the attack and the converter's smoothing settle.
    let win =
        |b: &[f32]| rms(&b[(cfg.sample_rate as usize / 2)..(cfg.sample_rate as usize / 2 + 8192)]);
    let (d, br) = (win(&dark), win(&bright));
    assert!(br > 0.02, "bright render near-silent ({br})");
    assert!(
        br > d * 1.4,
        "brightness should open the filter: dark {d}, bright {br}"
    );

    // Determinism: the same Good Button value renders bit-identically.
    let again = render_with_control(
        load_res(GOOD_BUTTON_JSON),
        cfg,
        1.0,
        Message::new("/brightness/in", Arg::F32(1.0), 0),
    );
    for (i, (x, y)) in bright.iter().zip(&again).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "non-deterministic at sample {i}");
    }
}

#[test]
fn auto_filter_base_plus_lfo_modulation_sounds_and_wobbles() {
    // ADR-0017 base-plus-modulation: a Signal `add` sums a base-cutoff CV (m2s) and an LFO
    // wobble, feeding the filter's Signal cutoff. The rig must sound; and turning the LFO
    // depth to 0 (a static cutoff) must change the output — which proves the LFO -> add ->
    // filter.cutoff modulation path is actually live, not bypassed.
    let cfg = AudioConfig::new(48_000.0, 256);

    let wobble = render_with_control(
        load_res(AUTO_FILTER_JSON),
        cfg,
        1.0,
        Message::new("/lfo/depth", Arg::F32(1500.0), 0),
    );
    let still = render_with_control(
        load_res(AUTO_FILTER_JSON),
        cfg,
        1.0,
        Message::new("/lfo/depth", Arg::F32(0.0), 0),
    );

    let peak = wobble.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    assert!(peak > 0.05, "auto-filter near-silent (peak {peak})");

    // Past the converter settle, the moving cutoff makes the wobble render diverge from the
    // static one. Compare RMS of the difference to the signal's own RMS.
    let sr = cfg.sample_rate as usize;
    let seg = sr / 2..sr / 2 + 8192;
    let diff: Vec<f32> = wobble[seg.clone()]
        .iter()
        .zip(&still[seg.clone()])
        .map(|(a, b)| a - b)
        .collect();
    let rel = rms(&diff) / rms(&wobble[seg]);
    assert!(
        rel > 0.1,
        "LFO modulation should visibly alter the output vs a static cutoff (relative diff {rel})"
    );

    // Determinism: the wobble render is bit-identical on a re-run.
    let again = render_with_control(
        load_res(AUTO_FILTER_JSON),
        cfg,
        1.0,
        Message::new("/lfo/depth", Arg::F32(1500.0), 0),
    );
    for (i, (x, y)) in wobble.iter().zip(&again).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "non-deterministic at sample {i}");
    }
}

#[test]
fn clock_makes_a_sample_accurate_metronome() {
    // The metronome rig (Clock beat-gate -> plucked envelope -> tone) clicks on every beat
    // with no external input: a click fires right after each beat boundary and the gap
    // between beats is silent. Beats are on the sample grid (no drift), and it's
    // deterministic.
    let cfg = AudioConfig::new(48_000.0, 256);
    let reg = Registry::builtin();

    let render_clicks = |seconds: f32| -> Vec<f32> {
        let graph = load(METRONOME_JSON, &reg).expect("load metronome.json");
        let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
        let mut r = Renderer::new(&plan);
        let blocks = (cfg.sample_rate * seconds) as usize / cfg.block_size;
        let mut buf = vec![0.0f32; cfg.block_size];
        let mut all = Vec::with_capacity(blocks * cfg.block_size);
        for _ in 0..blocks {
            r.render_block(&mut plan, &[], &mut buf);
            all.extend_from_slice(&buf);
        }
        all
    };

    let out = render_clicks(2.0);
    let spb = 24_000usize; // 120 BPM @ 48 kHz

    let peak = |w: &[f32]| w.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    for beat in 0..4 {
        let b = beat * spb;
        // A click fires in the window right after the beat boundary.
        let click_end = (b + 2_400).min(out.len());
        assert!(
            peak(&out[b..click_end]) > 0.05,
            "no click at beat {beat} (sample {b})"
        );
        // The remainder of the beat (sustain is 0) is silent.
        let gap = (b + 12_000)..(b + spb).min(out.len());
        if gap.start < gap.end {
            assert!(
                peak(&out[gap]) < 0.01,
                "beat {beat} should be silent before the next beat"
            );
        }
    }

    // Determinism holds for internally-clocked timing.
    let again = render_clicks(2.0);
    assert_eq!(out.len(), again.len());
    for (i, (x, y)) in out.iter().zip(&again).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "non-deterministic at sample {i}");
    }
}

#[test]
fn sequencer_emits_notes_through_a_voicer() {
    // The sequence rig (Clock beat-gate -> sequencer -> Voicer hosting sequence-voice sub-patches)
    // plays itself with no external input: the sequencer emits note Messages on the internal
    // message graph (ADR-0014), the Voicer drives a hosted voice per note, and each beat sounds
    // with a changing pitch. Deterministic, like the metronome.
    let cfg = AudioConfig::new(48_000.0, 256);
    let reg = Registry::builtin();

    let render_seq = |seconds: f32| -> Vec<f32> {
        let graph = load_instrument(SEQUENCE_JSON, &reg, &InstrumentsDir)
            .expect("load sequence.json")
            .graph;
        let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
        let mut r = Renderer::new(&plan);
        let blocks = (cfg.sample_rate * seconds) as usize / cfg.block_size;
        let mut buf = vec![0.0f32; cfg.block_size];
        let mut all = Vec::with_capacity(blocks * cfg.block_size);
        for _ in 0..blocks {
            r.render_block(&mut plan, &[], &mut buf);
            all.extend_from_slice(&buf);
        }
        all
    };

    let out = render_seq(2.0);
    let spb = 24_000usize; // 120 BPM @ 48 kHz

    // Each beat sounds (a note plays right after the beat boundary).
    let peak = |w: &[f32]| w.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    for beat in 0..4 {
        let b = beat * spb;
        let note_end = (b + 4_000).min(out.len());
        assert!(
            peak(&out[b..note_end]) > 0.02,
            "no note at beat {beat} (sample {b})"
        );
    }

    // Beats 0 and 1 are different pitches (60 then 62): the waveforms differ.
    let beat0 = &out[2_000..6_000];
    let beat1 = &out[(spb + 2_000)..(spb + 6_000)];
    let diff = beat0
        .iter()
        .zip(beat1)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(
        diff > 0.01,
        "beats 0 and 1 should be audibly different pitches"
    );

    // Determinism holds for the internally-clocked, message-driven sequence.
    let again = render_seq(2.0);
    assert_eq!(out.len(), again.len());
    for (i, (x, y)) in out.iter().zip(&again).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "non-deterministic at sample {i}");
    }
}

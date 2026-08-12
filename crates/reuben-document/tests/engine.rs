//! Engine construction from an instrument document, and the render behaviour that construction
//! path feeds. These drove `Engine` in `reuben-core` while the loader lived beside it; they moved
//! here with `from_document`, and they exercise both crates through their public surfaces only.

use reuben_core::{Arg, AudioConfig, Engine, Plan, Registry};
use reuben_document::engine::{from_document, FromDocumentError};
use reuben_document::resources::MemoryResolver;

/// The default rig, loaded as data exactly like native's embedded copy (its voice
/// sub-patch resolved in-memory) — the engine tests drive the real document path.
const DEFAULT_JSON: &str = include_str!("../../../instruments/default.json");
const DEFAULT_VOICE_JSON: &str = include_str!("../../../instruments/voices/default-voice.json");

fn default_resolver() -> MemoryResolver {
    let mut r = MemoryResolver::new();
    r.insert_text("voices/default-voice.json", DEFAULT_VOICE_JSON);
    r
}

fn default_engine(cfg: AudioConfig) -> Engine {
    let (engine, warnings) =
        from_document(DEFAULT_JSON, &Registry::builtin(), &default_resolver(), cfg)
            .expect("default.json is a valid instrument");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    engine
}

fn engine_with_note() -> Engine {
    let mut e = default_engine(AudioConfig::new(48_000.0, 256));
    // Drive a note in through the real inbound boundary: flat OSC args -> typed `Arg::Note`,
    // driven by the voicer's note port type.
    e.queue_osc("/voicer/notes", &[Arg::F32(69.0), Arg::F32(1.0)]);
    e
}

fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
}

#[test]
fn default_rig_makes_a_tone() {
    let mut e = engine_with_note();
    // The default rig is mono (broadcast) -> floors to 2 logical channels.
    assert_eq!(e.channels(), 2);
    let mut out = vec![0.0f32; 48_000]; // ~0.5 s interleaved stereo, not a block multiple
    e.fill(&mut out);
    assert!(peak(&out) > 0.05, "engine produced near-silence");
}

#[test]
fn from_document_surfaces_load_and_plan_errors() {
    let cfg = AudioConfig::new(48_000.0, 256);
    let result = from_document(
        "{ not json",
        &Registry::builtin(),
        &MemoryResolver::new(),
        cfg,
    );
    match result {
        Err(FromDocumentError::Load(_)) => {}
        Err(other) => panic!("expected a load error, got {other:?}"),
        Ok(_) => panic!("malformed document must not construct"),
    }

    // The Plan arm: a document that loads fine but whose graph cannot instantiate (a
    // wire cycle with no explicit unit-delay).
    const CYCLIC: &str = r#"{
      "format_version": 2,
      "instrument": "cycle",
      "nodes": [
        { "type": "add_f32_signal", "address": "/a", "inputs": { "a": { "from": "/b" } } },
        { "type": "add_f32_signal", "address": "/b", "inputs": { "a": { "from": "/a" } } },
        { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/a" } } }
      ]
    }"#;
    let result = from_document(CYCLIC, &Registry::builtin(), &MemoryResolver::new(), cfg);
    match result {
        Err(FromDocumentError::Plan(_)) => {}
        Err(other) => panic!("expected a plan error, got {other:?}"),
        Ok(_) => panic!("cyclic graph must not instantiate"),
    }
}

#[test]
fn fill_is_independent_of_chunk_size() {
    // One big fill must equal many ragged fills, sample-for-sample: the engine's block
    // boundary is decoupled from the caller's buffer size. Chunk sizes are in *frames*,
    // scaled to interleaved samples so each fill lands on a frame boundary.
    let ch = engine_with_note().channels();
    let total_frames = 5_000;
    let total = total_frames * ch;

    let mut whole = engine_with_note();
    let mut a = vec![0.0f32; total];
    whole.fill(&mut a);

    let mut chunked = engine_with_note();
    let mut b = vec![0.0f32; total];
    let mut i = 0;
    for step_frames in [37usize, 256, 1, 500, 129].iter().cycle() {
        if i >= total {
            break;
        }
        let end = (i + step_frames * ch).min(total);
        chunked.fill(&mut b[i..end]);
        i = end;
    }

    for (k, (x, y)) in a.iter().zip(&b).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "mismatch at sample {k}");
    }
}

/// A rig with a normal audio path (for a master tap) plus an `osc_out` sink at `/fb`.
fn osc_out_plan() -> Plan {
    use reuben_core::graph::Graph;
    use reuben_core::operators::{OscOut, Oscillator, Output};
    let mut g = Graph::new();
    let osc = g.add("/osc", Oscillator::new());
    let out = g.add("/out", Output::new());
    g.connect(osc, 0, out, 0);
    g.tap_output(out, 0);
    g.add("/fb", OscOut::new());
    Plan::instantiate(g, AudioConfig::new(48_000.0, 256)).expect("instantiate")
}

#[test]
fn outbound_messages_drain_after_fill() {
    // A value addressed to the sink's node routes in and comes back out on the outbound
    // route, stamped with the node address. The sink's input is the type-agnostic
    // pass-through: a single primitive atom crosses the inbound boundary
    // verbatim and echoes out unchanged — the OSC loopback path.
    let mut e = Engine::new(osc_out_plan());
    e.queue_osc("/fb/in", &[Arg::F32(0.5)]);
    let mut out = vec![0.0f32; e.block_size() * e.channels()];
    e.fill(&mut out);

    let drained: Vec<_> = e.drain_outbound().collect();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].address, "/fb");
    assert_eq!(drained[0].arg, Arg::F32(0.5));
    // Drained once: the next fill (no input) yields nothing.
    e.fill(&mut out);
    assert_eq!(e.drain_outbound().count(), 0);
}

#[test]
fn outbound_string_echoes_intact_after_fill() {
    // The first externally-admitted string on the engine path: a single
    // `Str` atom queued at the inbound boundary crosses verbatim, routes through the sink's
    // pass-through input, and drains outbound with the string intact and the sink's node
    // address stamped — the end-to-end string loopback.
    let mut e = Engine::new(osc_out_plan());
    e.queue_osc("/fb/in", &[Arg::Str("hello".into())]);
    let mut out = vec![0.0f32; e.block_size() * e.channels()];
    e.fill(&mut out);

    let drained: Vec<_> = e.drain_outbound().collect();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].address, "/fb");
    assert_eq!(drained[0].arg, Arg::Str("hello".into()));
    // Drained once: the next fill (no input) yields nothing.
    e.fill(&mut out);
    assert_eq!(e.drain_outbound().count(), 0);
}

/// A one-pipe passthrough bound to logical input channel 0 (P3).
fn input_engine(block_size: usize) -> Engine {
    const MIC: &str = r#"{
      "format_version": 2,
      "instrument": "mic_through",
      "interface": {
        "inputs":  { "mic": { "type": "f32_buffer", "channel": 0 } },
        "outputs": { "main": { "from": "/out.audio" } }
      },
      "nodes": [
        { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/mic" } } }
      ]
    }"#;
    let graph = reuben_document::load(MIC, &Registry::builtin()).expect("load");
    let plan =
        Plan::instantiate(graph, AudioConfig::new(48_000.0, block_size)).expect("instantiate");
    Engine::new(plan)
}

/// Deterministic mono input as a pure function of the global frame index.
fn in_sig(global_frame: usize) -> f32 {
    (global_frame % 89) as f32 / 89.0 - 0.5
}

#[test]
fn fill_duplex_stages_input_one_block_ahead() {
    // The alignment pin (see `fill_duplex` docs): the block rendered at global frame k*B
    // consumes input frames [(k-1)*B, k*B), so a bound pipe echoes the input with exactly
    // one core block of latency — and the first block is silence, whatever the input.
    let mut e = input_engine(128);
    assert_eq!(e.input_channels(), 1);
    let b = e.block_size();
    let ch = e.channels();
    let frames = 3 * b;
    let input: Vec<f32> = (0..frames).map(in_sig).collect(); // 1 input channel
    let mut out = vec![0.0f32; frames * ch];
    e.fill_duplex(&input, &mut out);
    for f in 0..frames {
        let expect = if f < b { 0.0 } else { in_sig(f - b) };
        assert_eq!(
            out[f * ch].to_bits(),
            expect.to_bits(),
            "frame {f}: input must land exactly one block later"
        );
    }
}

#[test]
fn fill_duplex_then_empty_input_stages_silence_not_stale_samples() {
    // The `in_dirty` stale-input pin (see `fill_duplex` docs): once real input has been
    // staged, a later empty-input call (`fill()`) must re-stage zeros so the last block
    // of live input never re-enters the render — the obvious refactor (early-return past
    // the staging loop when `input.is_empty()`, exactly what the pre-dirty path does)
    // would loop the final block of mic audio forever.
    let mut e = input_engine(128);
    let b = e.block_size();
    let ch = e.channels();
    // Stage two blocks of real input. One-block-ahead alignment: output block 0 is
    // silence, block 1 echoes input block 0 — and `in_scratch` now holds input block 1,
    // i.e. in_sig(b..2b), staged but not yet rendered.
    let input: Vec<f32> = (0..2 * b).map(in_sig).collect();
    let mut out = vec![0.0f32; 2 * b * ch];
    e.fill_duplex(&input, &mut out);

    // Switch to the no-input path. The first post-switch block legitimately echoes the
    // last real input block — that's the one-block latency playing out what was already
    // staged, not staleness.
    let mut post = vec![9.0f32; b * ch];
    e.fill(&mut post);
    for f in 0..b {
        assert_eq!(
            post[f * ch].to_bits(),
            in_sig(b + f).to_bits(),
            "frame {f}: the already-staged input block must still play out"
        );
    }
    // The second post-switch block reads what `fill()` staged: exact zeros — NOT a
    // replay of `in_scratch`'s previous (real-input) contents.
    e.fill(&mut post);
    for f in 0..b {
        assert_eq!(
            post[f * ch].to_bits(),
            0.0f32.to_bits(),
            "frame {f}: empty input after real input must stage silence, not stale samples"
        );
    }
}

#[test]
fn fill_duplex_is_independent_of_chunk_size() {
    // The input-side analog of `fill_is_independent_of_chunk_size`: one big duplex fill
    // must equal many ragged ones sample-for-sample — output stays a pure function of the
    // global frame index however input+output arrive.
    let probe = input_engine(128);
    let ch = probe.channels();
    let in_ch = probe.input_channels();
    let total_frames = 5_000;

    let input: Vec<f32> = (0..total_frames * in_ch)
        .map(|i| in_sig(i / in_ch))
        .collect();

    let mut whole = input_engine(128);
    let mut a = vec![0.0f32; total_frames * ch];
    whole.fill_duplex(&input, &mut a);

    let mut chunked = input_engine(128);
    let mut b = vec![0.0f32; total_frames * ch];
    let mut f = 0;
    for step in [37usize, 256, 1, 500, 129].iter().cycle() {
        if f >= total_frames {
            break;
        }
        let end = (f + step).min(total_frames);
        chunked.fill_duplex(&input[f * in_ch..end * in_ch], &mut b[f * ch..end * ch]);
        f = end;
    }

    for (k, (x, y)) in a.iter().zip(&b).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "mismatch at sample {k}");
    }
}

#[test]
fn fill_without_input_is_silence_for_a_bound_patch() {
    // The no-input convenience on an input-bound patch reads zeros — the
    // exact behavior every pre-P5 caller (no input stream yet) gets.
    let mut e = input_engine(256);
    let mut out = vec![1.0f32; 2048 * e.channels()];
    e.fill(&mut out);
    assert!(
        out.iter().all(|&s| s == 0.0),
        "unfed input pipe must be silent"
    );
}

#[test]
fn queued_messages_are_consumed_once() {
    // After a block renders, the queue is empty (the note isn't re-sent every block).
    let mut e = engine_with_note();
    let mut out = vec![0.0f32; e.block_size() * e.channels()];
    e.fill(&mut out);
    assert_eq!(e.pending_len(), 0, "pending messages not drained");
}

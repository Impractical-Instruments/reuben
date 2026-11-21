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
fn from_document_refuses_a_malformed_or_cyclic_document_at_load() {
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

    // A wire cycle (no explicit unit-delay) is found by the load, from the wires, before the
    // Plan arm's own cycle check could run: that arm is a backstop for a graph built by hand.
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
        Err(FromDocumentError::Load(errors)) => assert!(
            errors.to_string().contains("cycle through /a, /b"),
            "{errors}"
        ),
        Err(other) => panic!("expected a load error, got {other:?}"),
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

#[test]
fn control_that_cannot_land_is_dropped_without_touching_the_render() {
    // An address no port answers, and a real port sent arguments it cannot take, are both dropped
    // at `queue_osc` — nothing waits for the next block — and the render is bit-identical to one
    // that was never sent them.
    let cfg = AudioConfig::new(48_000.0, 256);
    let mut sent = engine_with_note();
    let mut clean = engine_with_note();
    sent.queue_osc("/nowhere/at/all", &[Arg::F32(1.0)]);
    sent.queue_osc("/voicer", &[Arg::F32(1.0)]);
    sent.queue_osc("/voicer/notes", &[Arg::Str("loud".into())]);
    sent.queue_osc("/voicer/notes", &[]);
    assert_eq!(sent.pending_len(), clean.pending_len());

    // The typed form: an unknown address and a payload the port's kind cannot hold.
    sent.queue(reuben_core::Message::float("/nowhere/at/all", 1.0, 0));
    sent.queue(reuben_core::Message::float("/voicer/notes", 1.0, 0));

    let mut a = vec![0.0f32; 8 * cfg.block_size * sent.channels()];
    let mut b = a.clone();
    sent.fill(&mut a);
    clean.fill(&mut b);
    assert_eq!(sent.pending_len(), 0);
    assert!(
        a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()),
        "dropped control changed the render"
    );
}

/// A filter with an enum `mode`, a dissolved `cut` pipe, and an `osc_out` sink fed by an `echo`
/// pipe: the shapes the shipped instruments lack (enum ports, outbound traffic).
const PARITY_JSON: &str = r#"{
  "format_version": 3, "instrument": "parity",
  "interface": {
    "inputs": {
      "cut":  { "type": "f32_buffer", "default": 800, "min": 20, "max": 20000 },
      "echo": { "type": "f32", "default": 0 }
    },
    "outputs": { "main": { "from": "/out.audio" } }
  },
  "nodes": [
    { "type": "oscillator", "address": "/osc" },
    { "type": "filter", "address": "/filt",
      "inputs": { "audio": { "from": "/osc.audio" }, "cutoff": { "from": "/cut" } } },
    { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/filt.audio" } } },
    { "type": "osc_out", "address": "/fb", "inputs": { "in": { "from": "/echo" } } }
  ]
}"#;

/// A fixed-seed xorshift: the parity sweep must replay identically on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn f32(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (self.next() % 10_000) as f32 / 10_000.0 * (hi - lo)
    }
}

#[test]
fn queued_control_renders_exactly_as_the_addressed_render_path() {
    // The Engine resolves control when it is queued; `Renderer::render_block_multi` resolves the
    // same Messages while routing. Random traffic — real ports, dissolved-pipe aliases, garbage
    // addresses, every arg shape, both queueing entry points — must render bit-identically and
    // send identical outbound Messages either way.
    use reuben_core::render::Renderer;
    use reuben_core::vocab::pitch::{Note, Pitch};
    use reuben_core::Message;

    const CHORD_JSON: &str = include_str!("../../../instruments/chord-player.json");
    const CHORD_VOICE_JSON: &str =
        include_str!("../../../instruments/voices/chord-player-voice.json");
    const BLOCKS: usize = 300;
    let cfg = AudioConfig::new(48_000.0, 128);
    let mut resolver = default_resolver();
    resolver.insert_text("voices/chord-player-voice.json", CHORD_VOICE_JSON);

    let strs: Vec<Arg> = ["Lp", "Bp", "Smooth", "bogus", ""]
        .iter()
        .map(|s| Arg::Str((*s).into()))
        .collect();
    let (mut aliases, mut outbound_seen) = (0, 0);
    for (name, doc) in [
        ("default", DEFAULT_JSON),
        ("chord-player", CHORD_JSON),
        ("parity", PARITY_JSON),
    ] {
        let load = || {
            reuben_document::load_instrument(doc, &Registry::builtin(), &resolver)
                .expect("loads")
                .graph
        };
        let mut engine = Engine::new(Plan::instantiate(load(), cfg).expect("instantiate"));
        let mut plan = Plan::instantiate(load(), cfg).expect("instantiate");
        let mut renderer = Renderer::new(&plan);

        let mut addresses: Vec<String> = plan
            .input_aliases()
            .iter()
            .map(|a| format!("{}/{}", a.address, a.port.name))
            .collect();
        aliases += addresses.len();
        for n in plan.nodes() {
            addresses.extend(
                n.descriptor
                    .inputs
                    .iter()
                    .map(|p| format!("{}/{}", n.address, p.name)),
            );
        }
        addresses.extend(["/nowhere", "/", "", "/out", "/voicer/notes/extra"].map(String::from));

        let ch = engine.channels();
        let mut out = vec![0.0f32; cfg.block_size * ch];
        let mut master = vec![vec![0.0f32; cfg.block_size]; ch];
        let mut outbound: Vec<Message> = Vec::new();
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for block in 0..BLOCKS {
            let mut messages = Vec::new();
            for _ in 0..rng.below(6) {
                let address = addresses[rng.below(addresses.len())].clone();
                if rng.below(2) == 0 {
                    let args: Vec<Arg> = match rng.below(6) {
                        0 => vec![Arg::F32(rng.f32(0.0, 1.0))],
                        1 => vec![Arg::I32(rng.below(8) as i32)],
                        2 => vec![Arg::F32(rng.f32(40.0, 80.0)), Arg::F32(rng.below(2) as f32)],
                        3 => vec![strs[rng.below(strs.len())].clone()],
                        4 => vec![],
                        _ => vec![Arg::I32(1), Arg::F32(0.5), Arg::F32(0.5)],
                    };
                    engine.queue_osc(&address, &args);
                    messages.extend(plan.osc_in_message(&address, &args));
                } else {
                    let arg = match rng.below(5) {
                        0 => Arg::F32(rng.f32(0.0, 1.0)),
                        1 => Arg::I32(rng.below(8) as i32),
                        2 => Arg::Note(Note::new(
                            Pitch::Absolute(rng.f32(40.0, 80.0)),
                            rng.below(2) as f32,
                        )),
                        3 => Arg::Enum(rng.below(4) as u32),
                        _ => strs[rng.below(strs.len())].clone(),
                    };
                    let msg = Message::new(address, arg, rng.below(cfg.block_size));
                    engine.queue(msg.clone());
                    messages.push(msg);
                }
            }

            engine.fill(&mut out);
            renderer.render_block_multi(&mut plan, &messages, &[], &mut master, &mut outbound);
            for f in 0..cfg.block_size {
                for c in 0..ch {
                    assert_eq!(
                        out[f * ch + c].to_bits(),
                        master[c][f].to_bits(),
                        "{name}: block {block} frame {f} channel {c} diverged"
                    );
                }
            }
            let sent: Vec<Message> = engine.drain_outbound().collect();
            assert_eq!(sent, outbound, "{name}: block {block} outbound diverged");
            outbound_seen += sent.len();
            outbound.clear();
        }
    }
    assert!(aliases > 0, "the sweep must reach a dissolved-pipe alias");
    assert!(outbound_seen > 0, "the sweep must send outbound traffic");
}

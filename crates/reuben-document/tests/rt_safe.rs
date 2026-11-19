//! Realtime-safety invariant: once warmed up, `Renderer::render_block` performs **zero**
//! heap allocation — neither while sustaining a held note nor while delivering note
//! messages (events are zero-copy views). A counting allocator makes any allocation on
//! the audio path observable.
//!
//! One layer up, [`Engine::queue_osc`] + [`Engine::fill`] — the route a host's flat-arg control
//! reaches the renderer through — touches the heap not at all. [`Engine::queue`] takes a Message
//! its caller built, so it allocates nothing and frees only that Message's address `String`.
//!
//! This file is its own test binary. Counting is armed per-thread by the shared [`rt_alloc`]
//! harness — each measured window arms this thread only around the render loop under test —
//! so an allocation on a libtest harness thread (or a sibling test) cannot interleave into a
//! window and perturb the count under parallel load.

mod rt_alloc;

use rt_alloc::{assert_counter_is_live, measure, Counting};

use std::sync::Arc;

use reuben_core::message::{Arg, Message};
use reuben_core::operators::{Clock, Output, SamplePlayer};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::{ResolvedRefs, ResourceStore, SampleBuffer};
use reuben_core::vocab::pitch::{Note, Pitch};
use reuben_core::{AudioConfig, Engine, Graph, Registry};
use reuben_document::load_instrument;
use reuben_document::resources::{ResolveError, ResourceResolver};

const DEFAULT_JSON: &str = include_str!("../../../instruments/default.json");

/// A filesystem resolver rooted at the frozen `tests/fixtures/` tree, so `default.json`'s `voice`
/// instrument-resource resolves to its on-disk voice patch.
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

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn render_block_is_allocation_free_after_warmup() {
    let cfg = AudioConfig::new(48_000.0, 256);

    // (1) The hosted-voice Voicer: playing `default.json` renders a voice sub-patch per
    // voice through the re-entrant `render_plan` over each voice's own arena. That nested render path
    // — and the sparse `freq`/`gate` message buffer the Voicer rebuilds each block — must be
    // allocation-free in steady state, both sustaining a held note and on message-bearing blocks.
    let graph = load_instrument(DEFAULT_JSON, &Registry::builtin(), &InstrumentsDir)
        .expect("load default.json")
        .graph;
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut out = vec![0.0f32; cfg.block_size];

    // Build the note-on message up front — its address String allocates here, off the measured path.
    let note_on = [Message::new(
        "/voicer/notes",
        Note::new(Pitch::Absolute(60.0), 1.0),
        0,
    )];

    // Warm up: deliver a note and render enough blocks to grow every internal scratch buffer (the
    // Voicer's per-voice message buffer + each sub-plan's render scratch) to steady-state capacity.
    r.render_block(&mut plan, &note_on, &mut out);
    for _ in 0..16 {
        r.render_block(&mut plan, &[], &mut out);
    }

    assert_counter_is_live();

    // Steady state: a held note with no new messages must not allocate.
    let held = measure(|| {
        for _ in 0..1000 {
            r.render_block(&mut plan, &[], &mut out);
        }
    })
    .allocs;
    assert_eq!(held, 0, "held-note render allocated {held} time(s)");

    // Message-bearing blocks must also not allocate — events are zero-copy views onto the caller's
    // Messages, and the Voicer reuses its per-voice message buffer (no per-block String alloc).
    let with_msgs = measure(|| {
        for _ in 0..100 {
            r.render_block(&mut plan, &note_on, &mut out);
        }
    })
    .allocs;
    assert_eq!(
        with_msgs, 0,
        "message-bearing render allocated {with_msgs} time(s)"
    );

    // (2) The sample player must be allocation-free too. Its `freq`/`gate`
    // are held **Value** controls, so a sample -> out rig is driven by routed Value messages directly
    // (the Voicer no longer emits per-Voice freq/gate buffers). The RT read goes through the store's
    // pure accessor and the Arc rides on the operator (an atomic bump, not a heap allocation), so
    // neither a sounding one-shot nor a retriggering note grows the heap.
    let mut g = Graph::new();
    let sample = g.add("/sample", SamplePlayer::new());
    g.set_value(sample, "root", &Arg::F32(69.0));
    let sink = g.add("/out", Output::new());

    // Bind the sample player to a synthetic resident buffer (a long ramp, so the one-shot sounds
    // across many blocks before reaching the end).
    let ramp: Vec<f32> = (0..48_000).map(|i| (i as f32 / 48_000.0) - 0.5).collect();
    let mut store = ResourceStore::new();
    let id = store.insert("s", SampleBuffer::new(vec![ramp], 48_000.0));
    let store = Arc::new(store);
    let mut refs = ResolvedRefs::new();
    refs.set("sample", id);
    g.nodes[sample].op.bind_resources(&store, &refs);

    g.connect(sample, 0, sink, 0);
    g.tap_output(sink, 0);

    let mut plan = Plan::instantiate(g, cfg).expect("instantiate sample rig");
    let mut r = Renderer::new(&plan);

    // Drive freq + open the gate (held Values). Built up front — addresses allocate off the path.
    let trigger = [
        Message::float("/sample/freq", 220.0, 0),
        Message::float("/sample/gate", 1.0, 0),
    ];

    // Warm up: fire the trigger, render enough to grow every scratch buffer to steady state.
    r.render_block(&mut plan, &trigger, &mut out);
    for _ in 0..16 {
        r.render_block(&mut plan, &[], &mut out);
    }

    // Steady state: a sounding/parked one-shot with no new messages must not allocate.
    let sample_held = measure(|| {
        for _ in 0..1000 {
            r.render_block(&mut plan, &[], &mut out);
        }
    })
    .allocs;
    assert_eq!(
        sample_held, 0,
        "sample rig steady-state allocated {sample_held} time(s)"
    );

    // Retriggering every block (gate rising edges) must also be allocation-free.
    let sample_retrig = measure(|| {
        for _ in 0..100 {
            r.render_block(&mut plan, &trigger, &mut out);
        }
    })
    .allocs;
    assert_eq!(
        sample_retrig, 0,
        "retriggering sample render allocated {sample_retrig} time(s)"
    );

    // Loop mode: a short region wraps (and crossfades) many times per second, the region moves
    // mid-play, and a gate fall runs the release fade. None of it may grow the heap.
    let loop_on = [
        Message::float("/sample/gate", 0.0, 0),
        Message::float("/sample/loop", 1.0, 0),
        Message::float("/sample/loop_start", 0.1, 0),
        Message::float("/sample/loop_end", 0.15, 0),
        Message::float("/sample/gate", 1.0, 64),
    ];
    let loop_move = [
        Message::float("/sample/loop_start", 0.5, 0),
        Message::float("/sample/loop_end", 0.52, 0),
    ];
    let gate_off = [Message::float("/sample/gate", 0.0, 0)];
    r.render_block(&mut plan, &loop_on, &mut out);
    r.render_block(&mut plan, &[], &mut out);
    let sample_loop = measure(|| {
        for _ in 0..500 {
            r.render_block(&mut plan, &[], &mut out);
        }
        r.render_block(&mut plan, &loop_move, &mut out);
        for _ in 0..500 {
            r.render_block(&mut plan, &[], &mut out);
        }
        r.render_block(&mut plan, &gate_off, &mut out);
        for _ in 0..16 {
            r.render_block(&mut plan, &[], &mut out);
        }
    })
    .allocs;
    assert_eq!(
        sample_loop, 0,
        "looping sample render allocated {sample_loop} time(s)"
    );

    // (3) A nested instrument (nesting P4): the subpatch dissolves at build into
    // ordinary flat nodes, so its steady-state render must be exactly as allocation-free as any
    // flat graph — inlining is a load-time transform, and the renderer never sees a boundary.
    struct Inline(&'static str);
    impl ResourceResolver for Inline {
        fn resolve(&self, s: &str) -> Result<SampleBuffer, ResolveError> {
            Err(ResolveError::NotFound(s.to_string()))
        }
        fn resolve_text(&self, _: &str) -> Result<String, ResolveError> {
            Ok(self.0.to_string())
        }
    }
    const TONE: &str = r#"{
        "instrument": "tone",
        "interface": { "inputs": { "freq": "/osc.freq" }, "outputs": { "audio": "/osc.audio" } },
        "nodes": [ { "type": "oscillator", "address": "/osc" } ]
    }"#;
    const NESTED: &str = r#"{
        "instrument": "nested",
        "resources": { "tone": "tone.json" },
        "nodes": [
            { "type": "subpatch", "address": "/a", "patch": "tone", "inputs": { "freq": 220.0 } },
            { "type": "subpatch", "address": "/b", "patch": "tone", "inputs": { "freq": 330.0 } }
        ],
        "outputs": [ { "node": "/a", "port": "audio" }, { "node": "/b", "port": "audio" } ]
    }"#;
    let graph = load_instrument(NESTED, &Registry::builtin(), &Inline(TONE))
        .expect("load nested instrument")
        .graph;
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate nested");
    let mut r = Renderer::new(&plan);
    for _ in 0..16 {
        r.render_block(&mut plan, &[], &mut out);
    }
    let nested_held = measure(|| {
        for _ in 0..1000 {
            r.render_block(&mut plan, &[], &mut out);
        }
    })
    .allocs;
    assert_eq!(
        nested_held, 0,
        "nested-instrument steady-state allocated {nested_held} time(s)"
    );

    // (4) The input master: a channel-bound input pipe's per-block feed is a
    // copy into a scratch buffer allocated at plan build — rendering with injected input must
    // be exactly as allocation-free as the output side.
    const MIC: &str = r#"{
        "format_version": 2,
        "instrument": "mic_through",
        "interface": {
            "inputs":  { "mic": { "type": "f32_buffer", "channel": 0 } },
            "outputs": { "main": { "from": "/echo.audio" } }
        },
        "nodes": [
            { "type": "delay", "address": "/echo", "inputs": { "audio": { "from": "/mic" } } }
        ]
    }"#;
    let graph = load_instrument(MIC, &Registry::builtin(), &InstrumentsDir)
        .expect("load input-bound instrument")
        .graph;
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate input-bound");
    assert_eq!(plan.config.input_channels, 1);
    let mut r = Renderer::new(&plan);
    let mut master: Vec<Vec<f32>> = vec![vec![0.0; cfg.block_size]; plan.config.channels];
    let mut outbound: Vec<reuben_core::message::Message> = Vec::with_capacity(8);
    let inputs: Vec<Vec<f32>> = vec![(0..cfg.block_size).map(|i| i as f32 * 1e-4).collect()];
    for _ in 0..16 {
        r.render_block_multi(&mut plan, &[], &inputs, &mut master, &mut outbound);
    }
    let input_held = measure(|| {
        for _ in 0..1000 {
            r.render_block_multi(&mut plan, &[], &inputs, &mut master, &mut outbound);
        }
    })
    .allocs;
    assert_eq!(
        input_held, 0,
        "input-master render allocated {input_held} time(s)"
    );

    // (5) A self-playing rig (emission machinery under sustained *internal* traffic):
    // `sequence.json`'s clock gate drives the sequencer, which emits note Messages into the
    // Voicer every beat — so the operator-emitted routing path (the `emit_scratch`/`emitted`
    // pool and each node's `events`/`held`/`materialize_writes` route vectors) runs every block
    // with no external messages at all. Sections (1)-(4) are emission-quiet in steady state or
    // drive messages from outside through `route_messages`; this is the only coverage of the
    // internal emission fan-out (note → Voicer retrigger → hosted-voice attack/release) staying
    // allocation-free once every pool has grown to steady capacity.
    let graph = load_instrument(
        include_str!("fixtures/sequence.json"),
        &Registry::builtin(),
        &InstrumentsDir,
    )
    .expect("load sequence.json")
    .graph;
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate sequence");
    let mut r = Renderer::new(&plan);

    // Warm up across the whole 8-step loop: at 120 BPM a beat is 24 000 frames ≈ 94 blocks of
    // 256, so 800 blocks covers all 8 steps, all 4 hosted voices, and every attack/release —
    // growing every scratch/pool to its steady-state high-water mark. Sanity-check the rig is
    // genuinely self-playing (a silent render would make the zero-alloc assertion vacuous).
    let mut sounded = false;
    for _ in 0..800 {
        r.render_block(&mut plan, &[], &mut out);
        sounded |= out.iter().any(|&s| s != 0.0);
    }
    assert!(
        sounded,
        "the sequenced rig must produce audio during warmup"
    );

    // Steady state: another 800 blocks — multiple beats, so note emissions, event routing, and
    // voice retriggers all happen *inside* the measured window — must not allocate.
    let sequenced = measure(|| {
        for _ in 0..800 {
            r.render_block(&mut plan, &[], &mut out);
        }
    })
    .allocs;
    assert_eq!(
        sequenced, 0,
        "sequenced-rig steady-state render allocated {sequenced} time(s)"
    );
}

/// Reads a voice patch from the repo `instruments/` tree, so `chord-player.json`'s
/// `chord-player-voice` resource resolves.
struct RepoInstruments;

impl ResourceResolver for RepoInstruments {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        let path = format!("{}/../../instruments/{source}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).map_err(|e| ResolveError::NotFound(format!("{path}: {e}")))
    }
}

/// Renders `bursts` (each one block's Messages, built before the call) one block apiece, with
/// three quiet blocks after each, and returns the allocations counted across all of them. Each
/// burst is rendered twice first, unmeasured, so every retained buffer reaches the high-water mark
/// the burst sets.
fn burst_allocs(
    plan: &mut Plan,
    r: &mut Renderer,
    out: &mut [f32],
    bursts: &[Vec<Message>],
) -> usize {
    let mut play = |plan: &mut Plan, r: &mut Renderer| {
        for burst in bursts {
            r.render_block(plan, burst, out);
            for _ in 0..3 {
                r.render_block(plan, &[], out);
            }
        }
    };
    play(plan, r);
    play(plan, r);
    measure(|| play(plan, r)).allocs
}

#[test]
fn an_event_burst_on_one_port_is_allocation_free_after_warmup() {
    let cfg = AudioConfig::new(48_000.0, 256);
    assert_counter_is_live();

    // External Messages: N notes on the Voicer's one `notes` port, all in one block. Past ~170
    // notes the Voicer's own note snapshot outgrows a stable sort's stack scratch, and at 2000 each
    // voice's `freq`/`gate` materialize writes do too, so the large sizes are not redundant.
    let graph = load_instrument(DEFAULT_JSON, &Registry::builtin(), &InstrumentsDir)
        .expect("load default.json")
        .graph;
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut out = vec![0.0f32; cfg.block_size];
    for n in [5usize, 8, 64, 100, 256, 2000] {
        // Note-ons then note-offs, so every repeat retriggers voices rather than holding them.
        let bursts: Vec<Vec<Message>> = [1.0, 0.0]
            .iter()
            .map(|&vel| {
                (0..n)
                    .map(|i| {
                        let pitch = Pitch::Absolute(36.0 + (i % 48) as f32);
                        Message::new("/voicer/notes", Note::new(pitch, vel), i % cfg.block_size)
                    })
                    .collect()
            })
            .collect();
        let allocs = burst_allocs(&mut plan, &mut r, &mut out, &bursts);
        assert_eq!(
            allocs, 0,
            "a {n}-note burst on one port allocated {allocs} time(s)"
        );
    }

    // Node-emitted events: each `/chord/in` press makes the `chord` op emit a triad into the
    // Voicer, so k presses in one block land 3k emitted events on the Voicer's `notes` port.
    let graph = load_instrument(
        include_str!("../../../instruments/chord-player.json"),
        &Registry::builtin(),
        &RepoInstruments,
    )
    .expect("load chord-player.json")
    .graph;
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate chord-player");
    let mut r = Renderer::new(&plan);
    for presses in [2usize, 7, 34] {
        let bursts: Vec<Vec<Message>> = [1.0, 0.0]
            .iter()
            .map(|&vel| {
                (0..presses)
                    .map(|i| {
                        let root = Pitch::Degree((i % 7) as i32);
                        Message::new("/chord/in", Note::new(root, vel), 0)
                    })
                    .collect()
            })
            .collect();
        let allocs = burst_allocs(&mut plan, &mut r, &mut out, &bursts);
        assert_eq!(
            allocs,
            0,
            "{presses} chord presses ({} emitted notes on one port) allocated {allocs} time(s)",
            presses * 3
        );
    }

    // Note processors between a source and the Voicer: every note passes through `transpose`
    // and then `snap`, so each one carries the whole burst on its `notes` port.
    const CHAIN: &str = r#"{
        "format_version": 3,
        "instrument": "note-chain",
        "resources": { "default-voice": "voices/default-voice.json" },
        "interface": { "outputs": { "out": { "from": "/out.audio" } } },
        "nodes": [
            { "type": "transpose", "address": "/t", "inputs": { "amount": 7.0 } },
            { "type": "snap", "address": "/s", "inputs": { "notes": { "from": "/t.notes" } } },
            { "type": "voicer", "address": "/voicer", "voice": "default-voice",
              "inputs": { "notes": { "from": "/s.notes" } } },
            { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/voicer.audio" } } }
        ]
    }"#;
    let graph = load_instrument(CHAIN, &Registry::builtin(), &RepoInstruments)
        .expect("load the transpose -> snap -> voicer chain")
        .graph;
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate the note chain");
    let mut r = Renderer::new(&plan);
    for n in [9usize, 17, 64, 256] {
        let bursts: Vec<Vec<Message>> = [1.0, 0.0]
            .iter()
            .map(|&vel| {
                (0..n)
                    .map(|i| {
                        let pitch = Pitch::Absolute(36.0 + (i % 48) as f32);
                        Message::new("/t/notes", Note::new(pitch, vel), i % cfg.block_size)
                    })
                    .collect()
            })
            .collect();
        let allocs = burst_allocs(&mut plan, &mut r, &mut out, &bursts);
        assert_eq!(
            allocs, 0,
            "a {n}-note burst through transpose and snap allocated {allocs} time(s)"
        );
    }

    // A burst on a `clock`'s `sync` port: every event is a phase reset at its frame.
    let mut g = Graph::new();
    let clock = g.add("/clock", Clock::new());
    let sink = g.add("/out", Output::new());
    g.connect(clock, 0, sink, 0);
    g.tap_output(sink, 0);
    let mut plan = Plan::instantiate(g, cfg).expect("instantiate clock rig");
    let mut r = Renderer::new(&plan);
    for n in [5usize, 64, 256] {
        let bursts = vec![(0..n)
            .map(|i| {
                Message::new(
                    "/clock/sync",
                    Note::new(Pitch::Degree(0), 1.0),
                    i % cfg.block_size,
                )
            })
            .collect::<Vec<_>>()];
        let allocs = burst_allocs(&mut plan, &mut r, &mut out, &bursts);
        assert_eq!(
            allocs, 0,
            "a {n}-event sync burst allocated {allocs} time(s)"
        );
    }
}

#[test]
fn engine_fill_with_queued_messages_is_allocation_free_after_warmup() {
    let cfg = AudioConfig::new(48_000.0, 256);
    let graph = load_instrument(DEFAULT_JSON, &Registry::builtin(), &InstrumentsDir)
        .expect("load default.json")
        .graph;
    let mut e = Engine::new(Plan::instantiate(graph, cfg).expect("instantiate"));
    // Exactly one block per `fill`, so every measured block carries the messages queued before it.
    let mut out = vec![0.0f32; cfg.block_size * e.channels()];

    const BLOCKS: usize = 100;
    const PER_BLOCK: usize = 2;
    // Every Message is built outside the measured window: its address `String` is the caller's
    // allocation, not the handoff's.
    let note =
        |pitch: f32| Message::new("/voicer/notes", Note::new(Pitch::Absolute(pitch), 1.0), 0);
    let mut batches: Vec<Vec<Message>> = (0..BLOCKS + 16)
        .map(|b| {
            (0..PER_BLOCK)
                .map(|i| note(48.0 + ((b + i) % 24) as f32))
                .collect()
        })
        .collect();
    let mut measured = batches.split_off(16);

    // Warm up with the same traffic, so every scratch buffer reaches its steady-state capacity.
    for batch in &mut batches {
        for m in batch.drain(..) {
            e.queue(m);
        }
        e.fill(&mut out);
    }

    let counts = measure(|| {
        for batch in &mut measured {
            for m in batch.drain(..) {
                e.queue(m);
            }
            e.fill(&mut out);
        }
    });
    assert_eq!(e.pending_len(), 0, "fill must consume every queued message");
    assert_eq!(
        counts.allocs, 0,
        "queue + fill of message-bearing blocks allocated {} time(s)",
        counts.allocs
    );
    // `queue` drops each Message's address `String` once it has resolved it — one free apiece is
    // the caller's allocation coming back. Anything above that is the handoff buffer being freed.
    assert_eq!(
        counts.frees,
        BLOCKS * PER_BLOCK,
        "queue + fill freed {} time(s) for {} consumed messages",
        counts.frees,
        BLOCKS * PER_BLOCK
    );
}

#[test]
fn engine_queue_osc_and_fill_is_heap_neutral_after_warmup() {
    let cfg = AudioConfig::new(48_000.0, 256);
    let graph = load_instrument(DEFAULT_JSON, &Registry::builtin(), &InstrumentsDir)
        .expect("load default.json")
        .graph;
    let mut e = Engine::new(Plan::instantiate(graph, cfg).expect("instantiate"));
    // Exactly one block per `fill`, so every measured block carries the messages queued before it.
    let mut out = vec![0.0f32; cfg.block_size * e.channels()];

    const BLOCKS: usize = 100;
    const PER_BLOCK: usize = 2;
    // Both inbound targets a host reaches: the dissolved `notes` pipe's address and the node port
    // it collapsed into. A note-on then a note-off per block, so the voice pool cycles.
    const ADDRESSES: [&str; PER_BLOCK] = ["/notes/in", "/voicer/notes"];
    let args = |b: usize, i: usize| {
        [
            Arg::F32(48.0 + (b % 24) as f32),
            Arg::F32((i == 0) as u8 as f32),
        ]
    };
    let mut block = |e: &mut Engine, b: usize| {
        for (i, address) in ADDRESSES.iter().enumerate() {
            e.queue_osc(address, &args(b, i));
        }
        e.fill(&mut out);
    };

    for b in 0..16 {
        block(&mut e, b);
    }

    assert_counter_is_live();
    let counts = measure(|| {
        for b in 0..BLOCKS {
            block(&mut e, b);
        }
    });
    assert_eq!(e.pending_len(), 0, "fill must consume every queued message");
    assert_eq!(
        (counts.allocs, counts.frees),
        (0, 0),
        "{BLOCKS} blocks of {PER_BLOCK} queue_osc + fill touched the heap"
    );
}

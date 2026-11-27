//! Realtime-safety invariant: once warmed up, `Renderer::render_block` performs **zero**
//! heap allocation — neither while sustaining a held note nor while delivering note
//! messages (events are zero-copy views). A counting allocator makes any allocation on
//! the audio path observable.
//!
//! One layer up, [`Engine::queue_osc`] + [`Engine::fill`] — the route a host's flat-arg control
//! reaches the renderer through — touches the heap not at all. [`Engine::queue`] takes a Message
//! its caller built, so it allocates nothing and frees only that Message's address `String`.
//!
//! Replacing a `str` input's prepared value frees nothing on the render thread either: the replaced
//! value goes to the retire queue, and the Coordinator frees it. The last tests pin that, the first
//! send after a Swap, the full-queue fallback, and the drain.
//!
//! This file is its own test binary. Counting is armed per-thread by the shared [`rt_alloc`]
//! harness — each measured window arms this thread only around the render loop under test —
//! so an allocation on a libtest harness thread (or a sibling test) cannot interleave into a
//! window and perturb the count under parallel load.

mod prepared;
mod rt_alloc;

use rt_alloc::{assert_counter_is_live, measure, Counting};

use std::sync::Arc;

use reuben_core::message::{Arg, Message};
use reuben_core::operators::{Clock, Output, SamplePlayer};
use reuben_core::plan::Plan;
use reuben_core::probe::ProbeSlots;
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
        "/notes/in",
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

/// Every probe of `slots` on, and of every slot set a node in it hosts.
fn every_probe_on(slots: &ProbeSlots) {
    for node in &slots.nodes {
        node.set_on(true);
        if let Some(hosted) = &node.hosted {
            every_probe_on(hosted);
        }
    }
}

/// Every probe on adds a measure after each node and a commit per block; neither may touch the
/// heap.
#[test]
fn render_with_every_probe_on_allocates_nothing() {
    let cfg = AudioConfig::new(48_000.0, 256);
    let graph = load_instrument(DEFAULT_JSON, &Registry::builtin(), &InstrumentsDir)
        .expect("load default.json")
        .graph;
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
    assert!(!plan.probes.nodes.is_empty(), "nothing to probe");
    every_probe_on(&plan.probes);
    assert!(
        plan.probes.nodes.iter().any(|n| n.hosted.is_some()),
        "the Voicer hosts no slots, so its voices' measure and commit went unproved"
    );
    let mut r = Renderer::new(&plan);
    let mut out = vec![0.0f32; cfg.block_size];
    let note_on = [Message::new(
        "/notes/in",
        Note::new(Pitch::Absolute(60.0), 1.0),
        0,
    )];
    r.render_block(&mut plan, &note_on, &mut out);
    for _ in 0..16 {
        r.render_block(&mut plan, &[], &mut out);
    }

    assert_counter_is_live();

    let probed = measure(|| {
        for _ in 0..1000 {
            r.render_block(&mut plan, &[], &mut out);
        }
    })
    .allocs;
    assert_eq!(probed, 0, "probed render allocated {probed} time(s)");
    assert!(
        plan.probes.nodes.iter().any(|n| n.ports[0].read().0 > 0.0),
        "no probe measured anything, so the window proved nothing"
    );
    assert!(
        plan.probes
            .nodes
            .iter()
            .filter_map(|n| n.hosted.as_deref())
            .flat_map(|hosted| &hosted.nodes)
            .any(|n| n.ports[0].read().0 > 0.0),
        "no voice probe measured anything, so the window proved nothing for the voices"
    );

    // Measuring only reads a node's output before a later node inherits its arena slot, so a
    // probed render must match an unprobed one bit for bit, recycled slots included.
    let fresh = || {
        let graph = load_instrument(DEFAULT_JSON, &Registry::builtin(), &InstrumentsDir)
            .expect("load default.json")
            .graph;
        Plan::instantiate(graph, cfg).expect("instantiate")
    };
    let (mut plain, mut probed) = (fresh(), fresh());
    every_probe_on(&probed.probes);
    let (mut r_plain, mut r_probed) = (Renderer::new(&plain), Renderer::new(&probed));
    let (mut a, mut b) = (vec![0.0f32; cfg.block_size], vec![0.0f32; cfg.block_size]);
    let mut sounded = false;
    for block in 0..300 {
        let messages: &[Message] = if block == 0 { &note_on } else { &[] };
        r_plain.render_block(&mut plain, messages, &mut a);
        r_probed.render_block(&mut probed, messages, &mut b);
        sounded |= a.iter().any(|&s| s != 0.0);
        assert!(
            a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()),
            "probed render diverged from unprobed at block {block}"
        );
    }
    assert!(sounded, "a silent comparison proves nothing");
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
                        Message::new("/notes/in", Note::new(pitch, vel), i % cfg.block_size)
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
    let note = |pitch: f32| Message::new("/notes/in", Note::new(Pitch::Absolute(pitch), 1.0), 0);
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
    // Both inbound targets a host reaches: the dissolved `notes` pipe's address, and the node port
    // it collapsed into, which routing drops. A note-on then a note-off per block, so the voice
    // pool cycles.
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

/// A kept `str` pipe (two consumers) with a default, and a node literal, all `steps` inputs
/// (`prepared/mod.rs`): every place a prepared value is replaced on the render thread — a seed
/// overwritten by the pipe's first forward, the pipe's latch and dedup baseline, its forwarded
/// copy, each consumer's latch, the queued control copy.
const STR_RIG: &str = r#"{
  "format_version": 3, "instrument": "str_rig",
  "interface": { "inputs": { "pat": { "type": "str", "default": "1 2" } } },
  "nodes": [
    { "type": "steps", "address": "/a", "inputs": { "steps": { "from": "/pat" } } },
    { "type": "steps", "address": "/b", "inputs": { "steps": { "from": "/pat" } } },
    { "type": "steps", "address": "/direct", "inputs": { "steps": "3" } },
    { "type": "steps", "address": "/other", "inputs": { "steps": "4" } } ] }"#;

const STR_BLOCK: usize = 128;

fn str_rig() -> (
    reuben_document::coordinator::Coordinator,
    reuben_core::coordinator::RenderSlot,
) {
    let (coord, side, _) = reuben_document::coordinator::Coordinator::install_initial(
        STR_RIG,
        prepared::registry(),
        Box::new(reuben_document::MemoryResolver::new()),
        AudioConfig::new(48_000.0, STR_BLOCK),
    )
    .expect("initial install");
    (coord, reuben_core::coordinator::RenderSlot::new(side))
}

/// `text` prepared for a `steps` input, its source handle kept by the caller only if it holds the
/// returned `Arc`.
fn prepared_with(text: Arc<str>) -> Arg {
    Arg::Prepared(
        prepared::steps_preparer()
            .prepare(&text)
            .expect("test text compiles"),
    )
}

/// Replacing prepared values never frees on the render thread: not in a Plan's first block, where
/// the kept pipe forwards its default over each consumer's seed, not in a Swap's install window,
/// and not across repeated sends to one input whose sender lets go of each value at once. What
/// the render thread lets go of reaches the Coordinator's retire queue, and the Coordinator frees
/// it.
#[test]
fn replacing_prepared_values_frees_nothing_on_the_render_thread() {
    let (mut coord, mut slot) = str_rig();
    let retired = coord.retired();
    let mut out = vec![0.0f32; STR_BLOCK * slot.channels()];
    assert_counter_is_live();

    // The first block of a fresh Plan: the pipe forwards "1 2" over both consumers' latches. A
    // fed input holds a placeholder that owns nothing, so nothing is freed anywhere.
    let first = measure(|| slot.fill(&mut out));
    assert_eq!(first.frees, 0, "first block freed {} time(s)", first.frees);
    let freed_off = measure(|| {
        retired.drain();
    })
    .frees;
    assert_eq!(freed_off, 0, "the consumers' placeholders own nothing");

    // Grow each node's routed-held scratch to its steady-state size.
    for i in 0..4 {
        slot.queue_osc("/pat/in", &[prepared::steps_arg(&format!("{i}"))]);
        slot.queue_osc("/direct/steps", &[prepared::steps_arg(&format!("{i}"))]);
        slot.fill(&mut out);
    }
    retired.drain();

    // Repeated sends to the same inputs, each value built outside the window and held by nothing
    // but the engine once queued.
    const SENDS: usize = 64;
    let mut values: Vec<[Arg; 2]> = (0..SENDS)
        .map(|i| {
            [
                prepared::steps_arg(&format!("{i} {i}")),
                prepared::steps_arg(&format!("{i}")),
            ]
        })
        .collect();
    let mut freed_off = 0;
    let mut render = rt_alloc::Counts {
        allocs: 0,
        frees: 0,
    };
    for args in values.drain(..) {
        let c = measure(|| {
            slot.queue_osc("/pat/in", &args[..1]);
            slot.queue_osc("/direct/steps", &args[1..]);
            slot.fill(&mut out);
        });
        render.allocs += c.allocs;
        render.frees += c.frees;
        drop(args);
        freed_off += measure(|| {
            retired.drain();
        })
        .frees;
    }
    assert_eq!(
        (render.allocs, render.frees),
        (0, 0),
        "{SENDS} blocks of prepared sends touched the heap on the render thread"
    );
    // Each send replaces two values the test let go of: the pipe's (held by its latch, its dedup
    // baseline, both consumers' latches and a forwarded copy, all retired) and `/direct`'s. Each
    // value owns its `Arc`, its source text and its non-empty `Compiled` list, so it is freed in
    // exactly one drain, three frees apiece: fewer is a leak, more a double retire.
    const FREES_PER_VALUE: usize = 3;
    assert_eq!(
        freed_off,
        2 * FREES_PER_VALUE * SENDS,
        "each replaced value is freed exactly once, off the render thread"
    );

    // A Swap's install window: a fresh Plan's first block replaces the consumers' seeds again.
    assert!(coord.swap_document(STR_RIG).report.ok);
    let mut window = 0;
    for _ in 0..16 {
        window += measure(|| slot.fill(&mut out)).frees;
    }
    assert!(!slot.is_ramping(), "the swap installed inside the window");
    assert_eq!(window, 0, "the install window freed {window} time(s)");
    assert!(coord.try_reclaim().is_some(), "the retiree came home");
    assert!(retired.is_empty(), "the reclaim drained the retire queue");
    assert_eq!(retired.dropped_full(), 0);
}

/// The first send after a Swap replaces whatever the input holds then, which a Swap seeded off
/// the render thread: an authored default the edit left alone on a survivor nothing was sent to,
/// and a value the Swap carried from the retiring Engine. Neither replacement touches the heap on
/// the render thread.
#[test]
fn the_first_send_after_a_swap_touches_no_heap() {
    let (mut coord, mut slot) = str_rig();
    let retired = coord.retired();
    let mut out = vec![0.0f32; STR_BLOCK * slot.channels()];
    for i in 0..4 {
        slot.queue_osc("/pat/in", &[prepared::steps_arg(&format!("{i}"))]);
        slot.queue_osc("/direct/steps", &[prepared::steps_arg(&format!("{i}"))]);
        slot.queue_osc("/other/steps", &[prepared::steps_arg(&format!("{i}"))]);
        slot.fill(&mut out);
    }
    // Round 0 swaps the same document, so every input keeps what was sent. Each later round swaps
    // in a different `/other` literal, so `/other` holds that authored default while `/direct` and
    // `/pat` hold the values the Swap carried.
    assert_counter_is_live();
    for round in 0..3 {
        let doc = match round {
            0 => STR_RIG.to_string(),
            n => STR_RIG.replace(r#""steps": "4""#, &format!(r#""steps": "4 {n}""#)),
        };
        assert!(coord.swap_document(&doc).report.ok, "swap {round}");
        for _ in 0..16 {
            slot.fill(&mut out);
        }
        assert!(!slot.is_ramping(), "swap {round} installed");
        coord.try_reclaim();
        retired.drain();
        let other = if round == 0 {
            "a carried value"
        } else {
            "its authored default"
        };
        for (addr, holds) in [
            ("/direct/steps", "a carried value"),
            ("/pat/in", "a carried interface value"),
            ("/other/steps", other),
        ] {
            let send = [prepared::steps_arg("7 7")];
            let c = measure(|| {
                slot.queue_osc(addr, &send);
                slot.fill(&mut out);
            });
            assert_eq!(
                (c.allocs, c.frees),
                (0, 0),
                "swap {round}: the first send to {addr}, holding {holds}"
            );
        }
    }
    assert_eq!(retired.dropped_full(), 0);
}

/// A full retire queue drops the value it cannot take on the render thread, once per value, and
/// counts each.
#[test]
fn a_full_retire_queue_drops_on_the_render_thread_and_counts_it() {
    use reuben_core::coordinator::retire_queue;

    let graph = load_instrument(
        STR_RIG,
        &prepared::registry(),
        &reuben_document::MemoryResolver::new(),
    )
    .expect("loads")
    .graph;
    let mut e =
        Engine::new(Plan::instantiate(graph, AudioConfig::new(48_000.0, STR_BLOCK)).unwrap());
    const CAPACITY: usize = 4;
    let (queue, retired) = retire_queue(CAPACITY);
    e.set_retire_queue(queue);
    let mut out = vec![0.0f32; STR_BLOCK * e.channels()];
    for i in 0..4 {
        e.queue_osc("/direct/steps", &[prepared::steps_arg(&format!("{i}"))]);
        e.fill(&mut out);
        retired.drain();
    }
    e.fill(&mut out);
    retired.drain();
    assert_eq!(
        retired.dropped_full(),
        0,
        "drained every block, the ring never filled"
    );

    // Each send to `/direct` retires two values (its queued copy and the latch it replaces); with
    // nothing draining, the ring fills after two sends and every value past that is dropped here.
    const SENDS: usize = 10;
    let values: Vec<Arg> = (0..SENDS)
        .map(|i| prepared::steps_arg(&format!("{i}")))
        .collect();
    assert_counter_is_live();
    let mut frees = 0;
    for v in values {
        let args = [v];
        frees += measure(|| {
            e.queue_osc("/direct/steps", &args);
            e.fill(&mut out);
        })
        .frees;
    }
    let overflow = 2 * SENDS - CAPACITY;
    assert_eq!(retired.len(), CAPACITY, "the ring is full");
    assert_eq!(
        retired.dropped_full(),
        overflow as u64,
        "every refused value is counted"
    );
    // The refused values are the queued copy (its latch still holds it, so dropping it frees
    // nothing) and the replaced latch (held by nothing else, so dropping it frees).
    assert!(
        frees > 0,
        "the overflow is dropped on the render thread: {frees} free(s)"
    );
    assert_eq!(retired.drain(), CAPACITY);
}

/// The Coordinator's reclaim frees what the render thread retires, on the Coordinator's thread:
/// over many sends and Swaps nothing leaks, the queue never grows past what one step retires, and
/// nothing overflows.
#[test]
fn the_coordinator_frees_retired_values_off_the_render_thread_without_leak_or_growth() {
    use std::sync::{mpsc, Weak};

    enum Step {
        Send(Arg),
        Fill(usize),
    }

    let (mut coord, mut slot) = str_rig();
    let retired = coord.retired();
    let (steps, inbox) = mpsc::channel::<Step>();
    let (done, render_done) = mpsc::channel::<()>();
    // The render thread: applies each step, measuring only the engine's own work.
    let render = std::thread::spawn(move || {
        let mut out = vec![0.0f32; STR_BLOCK * slot.channels()];
        let mut frees = 0;
        while let Ok(step) = inbox.recv() {
            match step {
                Step::Send(v) => {
                    let args = [v];
                    frees += measure(|| {
                        slot.queue_osc("/pat/in", &args);
                        slot.fill(&mut out);
                    })
                    .frees;
                }
                Step::Fill(blocks) => {
                    for _ in 0..blocks {
                        frees += measure(|| slot.fill(&mut out)).frees;
                    }
                }
            }
            done.send(()).expect("the test is waiting");
        }
        frees
    });
    let step = |s: Step| {
        steps.send(s).expect("the render thread is running");
        render_done.recv().expect("the render thread answers");
    };

    const SENDS: usize = 400;
    let mut sent: Vec<Weak<str>> = Vec::with_capacity(SENDS);
    let mut peak = 0;
    for i in 0..SENDS {
        let s: Arc<str> = format!("{i}").into();
        sent.push(Arc::downgrade(&s));
        step(Step::Send(prepared_with(s)));
        if i % 40 == 20 {
            assert!(coord.swap_document(STR_RIG).report.ok, "swap {i}");
            step(Step::Fill(16));
        }
        peak = peak.max(retired.len());
        coord.try_reclaim();
        assert!(
            retired.is_empty(),
            "the reclaim drains everything retired so far"
        );
    }
    // One more block retires the last block's forwarded copy; the live Plan still latches the last
    // value sent, and every one before it is gone, its text with it.
    step(Step::Fill(2));
    coord.try_reclaim();
    let alive: Vec<usize> = sent
        .iter()
        .enumerate()
        .filter(|(_, w)| w.strong_count() > 0)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(alive, [SENDS - 1], "only the latched value survives");
    assert!(
        (1..=16).contains(&peak),
        "the queue holds one step's retirees, not a backlog: peak {peak}"
    );
    assert_eq!(retired.dropped_full(), 0);

    drop(steps);
    let render_frees = render.join().expect("the render thread finished");
    assert_eq!(render_frees, 0, "the render thread freed nothing");
}

/// A Plan's first block allocates and frees nothing on the render thread, in the initial install
/// and in every Swap's install window, with kept pipes forwarding their defaults: a `str` pipe and
/// an `f32` pipe, each into two consumers, so each consumer gets its first held change there.
#[test]
fn a_plans_first_block_touches_no_heap_with_kept_pipes_forwarding() {
    const KEPT: &str = r#"{
      "format_version": 3, "instrument": "kept_pipes",
      "interface": { "inputs": {
          "pat": { "type": "str", "default": "1 2" },
          "rate": { "type": "f32", "default": 2.0, "min": 0.01, "max": 20.0 } } },
      "nodes": [
        { "type": "steps", "address": "/a", "inputs": { "steps": { "from": "/pat" } } },
        { "type": "steps", "address": "/b", "inputs": { "steps": { "from": "/pat" } } },
        { "type": "lfo", "address": "/l1", "inputs": { "rate": { "from": "/rate" } } },
        { "type": "lfo", "address": "/l2", "inputs": { "rate": { "from": "/rate" } } } ] }"#;
    let (mut coord, side, _) = reuben_document::coordinator::Coordinator::install_initial(
        KEPT,
        prepared::registry(),
        Box::new(reuben_document::MemoryResolver::new()),
        AudioConfig::new(48_000.0, STR_BLOCK),
    )
    .expect("initial install");
    let mut slot = reuben_core::coordinator::RenderSlot::new(side);
    let mut out = vec![0.0f32; STR_BLOCK * slot.channels()];
    assert_counter_is_live();

    let first = measure(|| slot.fill(&mut out));
    assert_eq!(
        (first.allocs, first.frees),
        (0, 0),
        "the initial install's first block touched the heap"
    );
    for _ in 0..4 {
        slot.fill(&mut out);
    }

    for edit in 0..3 {
        assert!(coord.swap_document(KEPT).report.ok, "swap {edit}");
        let window = measure(|| {
            for _ in 0..16 {
                slot.fill(&mut out);
            }
        });
        assert!(
            !slot.is_ramping(),
            "swap {edit} installed inside the window"
        );
        assert_eq!(
            (window.allocs, window.frees),
            (0, 0),
            "swap {edit}'s install window touched the heap"
        );
        assert!(
            coord.try_reclaim().is_some(),
            "swap {edit}'s retiree came home"
        );
    }
}

/// A resolver rooted at the repo's `instruments/`, for the library instrument the pattern run plays.
struct LibraryDir;

impl ResourceResolver for LibraryDir {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        let path = format!("{}/../../instruments/{source}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).map_err(|e| ResolveError::NotFound(format!("{path}: {e}")))
    }
}

/// `text` compiled by the built-in `pattern` Operator's own preparer, as a live `send` prepares it.
fn pattern_arg(text: &str) -> Arg {
    let registry = Registry::builtin();
    let entry = registry.get("pattern").expect("`pattern` is built in");
    let pat = (entry.descriptor.inputs.iter())
        .find(|p| p.name == "pat")
        .and_then(|p| p.preparer())
        .expect("`pat` prepares its text");
    Arg::Prepared(pat.prepare(&Arc::from(text)).expect("the pattern compiles"))
}

/// The densest pattern there is (a full cycle query of stacked notes, overflowing the held-notes
/// list) at the Clock's top tempo and one beat per cycle, into a Voicer, from a fresh install's
/// first block at both host block sizes: nothing on the render thread touches the heap. A block
/// that crosses more than one cycle start (thousands of frames at this tempo) is past what the
/// pools hold, and grows them once.
#[test]
fn the_densest_pattern_touches_no_heap_from_a_cold_start() {
    let doc = include_str!("../../../instruments/pattern-melody.json")
        .replace(r#""max": 240.0"#, r#""max": 999.0"#)
        .replace(
            r#""phase": {
          "from": "/clock.phase"
        }"#,
            r#""phase": { "from": "/clock.phase" }, "beats_per_cycle": 1"#,
        );
    assert!(
        doc.contains("beats_per_cycle"),
        "the library instrument's layout moved"
    );
    let stack: Vec<String> = (0..64).map(|d| d.to_string()).collect();
    for block in [128, 256] {
        let (_coord, side, _) = reuben_document::coordinator::Coordinator::install_initial(
            &doc,
            Registry::builtin(),
            Box::new(LibraryDir),
            AudioConfig::new(48_000.0, block),
        )
        .expect("the library instrument installs");
        let mut slot = reuben_core::coordinator::RenderSlot::new(side);
        let mut out = vec![0.0f32; block * slot.channels()];
        let pattern = [pattern_arg(&stack.join(", "))];
        let tempo = [Arg::F32(999.0)];
        assert_counter_is_live();
        let mut peak = 0.0f32;
        // Four seconds: about 66 cycles of the full stack.
        let c = measure(|| {
            slot.queue_osc("/tempo/in", &tempo);
            slot.queue_osc("/pattern/in", &pattern);
            for _ in 0..(4 * 48_000 / block) {
                slot.fill(&mut out);
                peak = out.iter().fold(peak, |p, s| p.max(s.abs()));
            }
        });
        assert_eq!((c.allocs, c.frees), (0, 0), "block {block}");
        assert!(peak > 0.0, "block {block}: the pattern played");
    }
}

/// The `pattern` Operator over a long run: cycle queries, note-ons and note-offs, evictions from
/// a full held-notes list, and new patterns sent mid-cycle that swap in at the next cycle start.
/// None of it allocates or frees on the render thread; every pattern let go of is freed off it.
#[test]
fn a_long_pattern_run_with_replacements_touches_no_heap_on_the_render_thread() {
    const BLOCK: usize = 128;
    let doc = include_str!("../../../instruments/pattern-melody.json");
    let (coord, side, _) = reuben_document::coordinator::Coordinator::install_initial(
        doc,
        Registry::builtin(),
        Box::new(LibraryDir),
        AudioConfig::new(48_000.0, BLOCK),
    )
    .expect("the library instrument installs");
    let mut slot = reuben_core::coordinator::RenderSlot::new(side);
    let retired = coord.retired();
    let mut out = vec![0.0f32; BLOCK * slot.channels()];

    // 240 BPM: a cycle of four beats is one second, 375 blocks.
    slot.queue_osc("/tempo/in", &[Arg::F32(240.0)]);

    // Patterns alternating with one denser than the held-notes list (40 stacked notes), each built
    // outside the measured window.
    let stack: Vec<String> = (0..40).map(|d| d.to_string()).collect();
    let texts = [
        "0 [3 5] <7 ~> 2*2".to_string(),
        stack.join(", "),
        "0/1.5 [1 2]*4".to_string(),
        "0*32 ~ 4?".to_string(),
    ];
    const SENDS: usize = 24;
    let mut values: Vec<Arg> = (0..SENDS)
        .map(|i| pattern_arg(&texts[i % texts.len()]))
        .collect();
    assert_counter_is_live();
    let mut render = rt_alloc::Counts {
        allocs: 0,
        frees: 0,
    };
    let mut freed_off = 0;
    for v in values.drain(..) {
        let args = [v];
        // A send lands mid-cycle; 160 blocks later most sends have crossed a cycle start.
        let c = measure(|| {
            slot.queue_osc("/pattern/in", &args);
            for _ in 0..160 {
                slot.fill(&mut out);
            }
        });
        render.allocs += c.allocs;
        render.frees += c.frees;
        drop(args);
        freed_off += measure(|| {
            retired.drain();
        })
        .frees;
    }
    assert_eq!(
        (render.allocs, render.frees),
        (0, 0),
        "{SENDS} pattern sends over {} blocks touched the heap on the render thread",
        SENDS * 160
    );
    assert!(
        freed_off > 0,
        "the replaced patterns were freed off the render thread"
    );
    assert_eq!(retired.dropped_full(), 0);
}

/// A caller that queues past the Engine's control queue without waiting for room: the overflow is
/// dropped and counted, never grown into, from a fresh Engine's first block.
#[test]
fn engine_queue_osc_past_its_capacity_drops_and_counts_without_the_heap() {
    use reuben_core::engine::PENDING_CAP;

    const DOC: &str = r#"{ "format_version": 3, "instrument": "overflow",
        "interface": { "outputs": { "out": { "from": "/osc.audio" } } },
        "nodes": [
          { "type": "oscillator", "address": "/osc" },
          { "type": "abs_i32_value", "address": "/abs" } ] }"#;
    const OVER: usize = 10;
    let cfg = AudioConfig::new(48_000.0, 64);
    let graph = load_instrument(DOC, &Registry::builtin(), &InstrumentsDir)
        .expect("load")
        .graph;
    let mut e = Engine::new(Plan::instantiate(graph, cfg).expect("instantiate"));
    let mut out = vec![0.0f32; cfg.block_size * e.channels()];
    e.fill(&mut out);

    assert_counter_is_live();
    let q = measure(|| {
        for i in 0..PENDING_CAP + OVER {
            e.queue_osc("/abs/x", &[Arg::I32(i as i32)]);
        }
    });
    let f = measure(|| e.fill(&mut out));
    assert_eq!(((q.allocs, q.frees), (f.allocs, f.frees)), ((0, 0), (0, 0)));
    assert_eq!(e.take_dropped_queue_full(), OVER as u64);
    assert_eq!(e.pending_room(), PENDING_CAP, "the next block starts empty");
}

/// A full control queue's worth of messages to one node in a fresh Engine's first block, for each
/// kind of input routing stores: a held value, a materialized signal, and Stream events into a
/// Voicer. Routing and rendering them touches no heap.
#[test]
fn a_full_queue_to_one_node_routes_without_the_heap_from_the_first_block() {
    use reuben_core::engine::PENDING_CAP;

    const DOC: &str = r#"{ "format_version": 3, "instrument": "dense",
        "interface": { "outputs": { "out": { "from": "/filt.audio" } } },
        "nodes": [
          { "type": "oscillator", "address": "/osc" },
          { "type": "filter", "address": "/filt", "inputs": { "audio": { "from": "/osc.audio" } } },
          { "type": "abs_i32_value", "address": "/abs" } ] }"#;
    let fresh = |doc: &str| {
        let cfg = AudioConfig::new(48_000.0, 64);
        let graph = load_instrument(doc, &Registry::builtin(), &InstrumentsDir)
            .expect("load")
            .graph;
        Engine::new(Plan::instantiate(graph, cfg).expect("instantiate"))
    };
    type Args = fn(usize) -> Vec<Arg>;
    let cases: [(&str, &str, Args); 3] = [
        (DOC, "/abs/x", |i| vec![Arg::I32(i as i32)]),
        (DOC, "/filt/cutoff", |i| vec![Arg::F32(200.0 + i as f32)]),
        // Alternating note-on and note-off, so the pool both assigns and releases.
        (DEFAULT_JSON, "/notes/in", |i| {
            vec![Arg::F32(48.0 + (i % 24) as f32), Arg::F32((i % 2) as f32)]
        }),
    ];
    for (doc, address, args) in cases {
        let mut e = fresh(doc);
        let mut out = vec![0.0f32; e.block_size() * e.channels()];
        let msgs: Vec<Vec<Arg>> = (0..PENDING_CAP).map(args).collect();
        e.fill(&mut out);
        assert_counter_is_live();
        let c = measure(|| {
            for m in &msgs {
                e.queue_osc(address, m);
            }
            e.fill(&mut out);
        });
        assert_eq!(
            (c.allocs, c.frees),
            (0, 0),
            "{PENDING_CAP} messages to {address}"
        );
    }
}

/// A node as wide as the widest built-in Operator (`mix`: 16 audio inputs, all wired, and 16
/// gains) renders without spilling its per-segment port lists to the heap.
#[test]
fn the_widest_operator_renders_without_the_heap() {
    let wired: Vec<String> = (1..=16)
        .map(|i| format!(r#""in{i}": {{ "from": "/osc.audio" }}"#))
        .collect();
    let doc = format!(
        r#"{{ "format_version": 3, "instrument": "wide",
        "interface": {{ "outputs": {{ "out": {{ "from": "/mix.out" }} }} }},
        "nodes": [
          {{ "type": "oscillator", "address": "/osc" }},
          {{ "type": "mix", "address": "/mix", "inputs": {{ {} }} }} ] }}"#,
        wired.join(", ")
    );
    let cfg = AudioConfig::new(48_000.0, 64);
    let graph = load_instrument(&doc, &Registry::builtin(), &InstrumentsDir)
        .expect("load")
        .graph;
    let mut e = Engine::new(Plan::instantiate(graph, cfg).expect("instantiate"));
    let mut out = vec![0.0f32; cfg.block_size * e.channels()];
    e.fill(&mut out);
    assert_counter_is_live();
    let c = measure(|| {
        for _ in 0..16 {
            e.fill(&mut out);
        }
    });
    assert_eq!((c.allocs, c.frees), (0, 0));
}

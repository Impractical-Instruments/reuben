//! Realtime-safety invariant: once warmed up, `Renderer::render_block` performs **zero**
//! heap allocation — neither while sustaining a held note nor while delivering note
//! messages (events are zero-copy views). A process-global counting allocator makes any
//! allocation on the audio path observable.
//!
//! This file is its own test binary with a single test, so no sibling test runs
//! concurrently to perturb the global allocation counter.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use reuben_core::message::{Arg, Message};
use reuben_core::operators::{Output, SamplePlayer};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::{
    ResolveError, ResolvedRefs, ResourceResolver, ResourceStore, SampleBuffer,
};
use reuben_core::vocab::pitch::{Note, Pitch};
use reuben_core::{load_instrument, AudioConfig, Graph, Registry};

const DEFAULT_JSON: &str = include_str!("../../../instruments/default.json");

/// A filesystem resolver rooted at the repo `instruments/` dir, so `default.json`'s `voice`
/// instrument-resource (ADR-0032) resolves to its on-disk voice patch.
struct InstrumentsDir;

impl ResourceResolver for InstrumentsDir {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        let path = format!("{}/../../instruments/{source}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).map_err(|e| ResolveError::NotFound(format!("{path}: {e}")))
    }
}

/// Number of `alloc`/`realloc` calls since process start.
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

/// System allocator that counts allocations and reallocations (i.e. heap growth).
struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn render_block_is_allocation_free_after_warmup() {
    let cfg = AudioConfig::new(48_000.0, 256);

    // (1) The hosted-voice Voicer (ADR-0032): playing `default.json` renders a voice sub-patch per
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

    // Steady state: a held note with no new messages must not allocate.
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..1000 {
        r.render_block(&mut plan, &[], &mut out);
    }
    let held = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(held, 0, "held-note render allocated {held} time(s)");

    // Message-bearing blocks must also not allocate — events are zero-copy views onto the caller's
    // Messages, and the Voicer reuses its per-voice message buffer (no per-block String alloc).
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..100 {
        r.render_block(&mut plan, &note_on, &mut out);
    }
    let with_msgs = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        with_msgs, 0,
        "message-bearing render allocated {with_msgs} time(s)"
    );

    // (2) The sample player (ADR-0016) must be allocation-free too. Post-ADR-0031 its `freq`/`gate`
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
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..1000 {
        r.render_block(&mut plan, &[], &mut out);
    }
    let sample_held = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        sample_held, 0,
        "sample rig steady-state allocated {sample_held} time(s)"
    );

    // Retriggering every block (gate rising edges) must also be allocation-free.
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..100 {
        r.render_block(&mut plan, &trigger, &mut out);
    }
    let sample_retrig = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        sample_retrig, 0,
        "retriggering sample render allocated {sample_retrig} time(s)"
    );

    // (3) A nested instrument (ADR-0034, nesting P4): the subpatch dissolves at build into
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
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..1000 {
        r.render_block(&mut plan, &[], &mut out);
    }
    let nested_held = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        nested_held, 0,
        "nested-instrument steady-state allocated {nested_held} time(s)"
    );

    // (4) The input master (ADR-0038 §3, P3): a channel-bound input pipe's per-block feed is a
    // copy into a scratch buffer allocated at plan build — rendering with injected input must
    // be exactly as allocation-free as the output side (issue #180's RT-safety clause).
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
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..1000 {
        r.render_block_multi(&mut plan, &[], &inputs, &mut master, &mut outbound);
    }
    let input_held = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        input_held, 0,
        "input-master render allocated {input_held} time(s)"
    );

    // (5) A self-playing rig (ADR-0014's emission machinery under sustained *internal* traffic):
    // `sequence.json`'s clock gate drives the sequencer, which emits note Messages into the
    // Voicer every beat — so the operator-emitted routing path (the `emit_scratch`/`emitted`
    // pool and each node's `events`/`held`/`materialize_writes` route vectors) runs every block
    // with no external messages at all. Sections (1)-(4) are emission-quiet in steady state or
    // drive messages from outside through `route_messages`; this is the only coverage of the
    // internal emission fan-out (note → Voicer retrigger → hosted-voice attack/release) staying
    // allocation-free once every pool has grown to steady capacity.
    let graph = load_instrument(
        include_str!("../../../instruments/sequence.json"),
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
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..800 {
        r.render_block(&mut plan, &[], &mut out);
    }
    let sequenced = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        sequenced, 0,
        "sequenced-rig steady-state render allocated {sequenced} time(s)"
    );
}

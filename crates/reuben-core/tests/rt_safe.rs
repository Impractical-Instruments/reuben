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

use reuben_core::message::Message;
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
    g.set_param(sample, "root", 69.0);
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
}

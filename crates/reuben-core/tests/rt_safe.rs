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
use reuben_core::operators::{Output, SamplePlayer, Voicer};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::{ResolvedRefs, ResourceStore, SampleBuffer};
use reuben_core::vocab::pitch::{Note, Pitch};
use reuben_core::{load, AudioConfig, Graph, Registry};

const DEFAULT_JSON: &str = include_str!("../../../instruments/default.json");
const SEQUENCE_JSON: &str = include_str!("../../../instruments/sequence.json");
const SCALE_DEMO_JSON: &str = include_str!("../../../instruments/scale-demo.json");
const GOOD_BUTTON_JSON: &str = include_str!("../../../instruments/good-button.json");

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
    let graph = load(DEFAULT_JSON, &Registry::builtin()).expect("load default.json");
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut out = vec![0.0f32; cfg.block_size];

    // Build the note-on message up front — its address String allocates here, off the
    // measured render path.
    let note_on = [Message::new(
        "/voicer/notes",
        Note::new(Pitch::Absolute(60.0), 1.0),
        0,
    )];

    // Warm up: deliver a note and render enough blocks to grow every internal scratch
    // buffer (routes, bounds, out_scratch, order) to its steady-state capacity.
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

    // Message-bearing blocks must also not allocate — events are zero-copy views onto the
    // caller's Messages, not cloned.
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..100 {
        r.render_block(&mut plan, &note_on, &mut out);
    }
    let with_msgs = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        with_msgs, 0,
        "message-bearing render allocated {with_msgs} time(s)"
    );

    // Enum control messages (ADR-0028) must also be allocation-free: routing `/filter/mode "Hp"`
    // resolves the `Sym` arg to a variant index on the wire. The arg is borrowed, not cloned, so
    // the resolve touches no allocator. (The `Sym` String allocates here, off the measured path.)
    let mode_hp = [Message::new("/filter/mode", Arg::Str("Hp".into()), 0)];
    let mode_lp = [Message::new("/filter/mode", Arg::Str("Lp".into()), 0)];
    r.render_block(&mut plan, &mode_hp, &mut out); // warm the enum-route scratch
    let before = ALLOCS.load(Ordering::Relaxed);
    for i in 0..100 {
        // Flip the mode each block so the enum route resolves a fresh symbol every time.
        let msgs = if i % 2 == 0 { &mode_lp } else { &mode_hp };
        r.render_block(&mut plan, msgs, &mut out);
    }
    let with_enum = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        with_enum, 0,
        "enum-message render allocated {with_enum} time(s)"
    );

    // Operator-emitted messages (ADR-0014) must also be allocation-free in steady state:
    // the sequence rig has a sequencer emitting `note` Messages into a Voicer every beat.
    let graph = load(SEQUENCE_JSON, &Registry::builtin()).expect("load sequence.json");
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);

    // Warm up across more than a full beat (24000 frames @ 120 BPM) so every emit pool and
    // per-node event Vec reaches steady-state capacity, spanning several note on/off emits.
    for _ in 0..200 {
        r.render_block(&mut plan, &[], &mut out);
    }
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..1000 {
        r.render_block(&mut plan, &[], &mut out);
    }
    let emitting = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        emitting, 0,
        "emitting-sequencer render allocated {emitting} time(s)"
    );

    // The tonal-context bus (ADR-0015) must be allocation-free too: a publisher → context →
    // resolving-Voicer rig, both in steady state and across live context changes (the `Copy`
    // snapshot is a memcpy; reader slices reuse the precapped context pool).
    let graph = load(SCALE_DEMO_JSON, &Registry::builtin()).expect("load scale-demo.json");
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);

    // Build the key-change messages up front (their address Strings allocate here).
    let to_d = [Message::new("/context/root", Arg::F32(62.0), 0)];
    let to_c = [Message::new("/context/root", Arg::F32(60.0), 0)];

    for _ in 0..200 {
        r.render_block(&mut plan, &[], &mut out);
    }
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..1000 {
        r.render_block(&mut plan, &[], &mut out);
    }
    let ctx_steady = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        ctx_steady, 0,
        "context rig steady-state allocated {ctx_steady} time(s)"
    );

    // Warm up the change path too: the first publishes grow the context pool and reader
    // slice/bounds Vecs to their steady-state capacity (off the measured window).
    for i in 0..16 {
        let msgs = if i % 2 == 0 { &to_d } else { &to_c };
        r.render_block(&mut plan, msgs, &mut out);
    }
    let before = ALLOCS.load(Ordering::Relaxed);
    for i in 0..200 {
        // Alternate the key every block so the context node publishes a change each time.
        let msgs = if i % 2 == 0 { &to_d } else { &to_c };
        r.render_block(&mut plan, msgs, &mut out);
    }
    let ctx_changing = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        ctx_changing, 0,
        "context-changing render allocated {ctx_changing} time(s)"
    );

    // The math family + M→S converter (ADR-0017) must be allocation-free too: the Good Button
    // rig fans a `map` emit to two ranged `map`s into two `m2s` converters driving the filter's
    // Signal cutoff/resonance. Emits route into the precapped pool; converters write CV buffers.
    let graph = load(GOOD_BUTTON_JSON, &Registry::builtin()).expect("load good-button.json");
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);

    // Build the Good Button + note messages up front (their address Strings allocate here).
    let bright = [Message::new("/brightness/in", Arg::F32(1.0), 0)];
    let note = [Message::new(
        "/voicer/notes",
        Note::new(Pitch::Absolute(57.0), 1.0),
        0,
    )];

    r.render_block(&mut plan, &note, &mut out);
    for _ in 0..16 {
        r.render_block(&mut plan, &bright, &mut out);
    }
    // Steady state: the converters hold their last value with no new messages.
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..1000 {
        r.render_block(&mut plan, &[], &mut out);
    }
    let gb_steady = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        gb_steady, 0,
        "good-button steady-state allocated {gb_steady} time(s)"
    );

    // Sweeping the Good Button every block (a `map` emit fanned through the chain each block)
    // must also be allocation-free.
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..100 {
        r.render_block(&mut plan, &bright, &mut out);
    }
    let gb_sweeping = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        gb_sweeping, 0,
        "good-button sweeping allocated {gb_sweeping} time(s)"
    );

    // The sample player (ADR-0016) must be allocation-free too: a voicer -> sample -> out rig
    // bound to a resident buffer. The RT read goes through the store's pure accessor and the
    // Arc rides on the operator (cloned per call — an atomic bump, not a heap allocation), so
    // neither a sounding one-shot nor a retriggering note grows the heap.
    let mut g = Graph::new();
    let voicer = g.add("/voicer", Voicer::new());
    g.set_param(voicer, "voices", 1.0);
    let sample = g.add("/sample", SamplePlayer::new());
    g.set_param(sample, "root", 69.0);
    let sink = g.add("/out", Output::new());

    // Bind the sample player to a synthetic resident buffer (a long ramp, so the one-shot
    // sounds across many blocks before reaching the end).
    let ramp: Vec<f32> = (0..48_000).map(|i| (i as f32 / 48_000.0) - 0.5).collect();
    let mut store = ResourceStore::new();
    let id = store.insert("s", SampleBuffer::new(vec![ramp], 48_000.0));
    let store = Arc::new(store);
    let mut refs = ResolvedRefs::new();
    refs.set("sample", id);
    g.nodes[sample].op.bind_resources(&store, &refs);

    // voicer.freq(0) -> sample.freq(0), voicer.gate(1) -> sample.gate(1), sample.audio -> out.
    g.connect(voicer, 0, sample, 0);
    g.connect(voicer, 1, sample, 1);
    g.connect(sample, 0, sink, 0);
    g.tap_output(sink, 0);

    let mut plan = Plan::instantiate(g, cfg).expect("instantiate sample rig");
    let mut r = Renderer::new(&plan);

    // Warm up: fire a note, render enough to grow every scratch buffer to steady state.
    r.render_block(&mut plan, &note_on, &mut out);
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

    // Retriggering every block (gate edges) must also be allocation-free.
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..100 {
        r.render_block(&mut plan, &note_on, &mut out);
    }
    let sample_retrig = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        sample_retrig, 0,
        "retriggering sample render allocated {sample_retrig} time(s)"
    );
}

//! The render callback's control drain, under the counting allocator: every batch it takes goes
//! back to its producer, so nothing is allocated or freed on the render thread.

#[path = "../../reuben-document/tests/rt_alloc/mod.rs"]
mod rt_alloc;

use reuben_api::render::{Arg, AudioConfig, RenderSlot};
use reuben_native::audio::drain_control;
use reuben_native::control::{ring_pair, ControlDrain};
use reuben_native::diagnostics::Diagnostics;
use reuben_native::osc::{ControlBatch, OscIn};
use reuben_native::test_support::no_store;
use rt_alloc::{assert_counter_is_live, measure, Counting};

#[global_allocator]
static GLOBAL: Counting = Counting;

const DOC: &str = r#"{ "format_version": 3, "instrument": "drain",
    "interface": { "outputs": { "out": { "from": "/out.audio" } } },
    "nodes": [
      { "type": "oscillator", "address": "/osc" },
      { "type": "filter", "address": "/filt", "inputs": { "audio": { "from": "/osc.audio" } } },
      { "type": "abs_i32_value", "address": "/abs" },
      { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/filt.audio" } } } ] }"#;

/// An `F32`, an `I32` and an enum symbol as `Str`, so the `Arc<str>` is one the port lookup does
/// not keep.
fn gesture(mut batch: ControlBatch, i: usize) -> ControlBatch {
    batch.extend([
        OscIn::new("/filt/cutoff".to_string(), vec![Arg::F32(200.0 + i as f32)]),
        OscIn::new("/abs/x".to_string(), vec![Arg::I32(i as i32)]),
        OscIn::new(
            "/filt/mode".to_string(),
            vec![Arg::Str(
                if i.is_multiple_of(2) { "Hp" } else { "Bp" }.into(),
            )],
        ),
    ]);
    batch
}

/// Three-message batches, inside the Engine's preallocated control queue: this covers the drain and
/// its return path, not the Engine's queue growing past its first capacity.
#[test]
fn draining_control_neither_allocates_nor_frees_on_the_render_thread() {
    let cfg = AudioConfig::new(48_000.0, 64);
    let (_coordinator, side, _) =
        reuben_api::engine::install_initial(DOC, no_store(), cfg).expect("install");
    let mut slot = RenderSlot::new(side);
    let mut out = vec![0.0f32; cfg.block_size * slot.channels()];
    let diagnostics = Diagnostics::new();
    let (mut producer, ring) = ring_pair(Diagnostics::new());
    let mut control = ControlDrain::new(vec![ring]);

    for i in 0..8 {
        let batch = gesture(producer.batch(), i);
        producer.push(batch).unwrap();
        drain_control(&mut control, &mut slot, &diagnostics);
        slot.fill(&mut out);
    }

    // Two gestures per block for 40 blocks: 80 batches, which would cross an unbounded `mpsc`
    // channel's 31-slot block twice.
    const BLOCKS: usize = 40;
    assert_counter_is_live();
    let mut total = rt_alloc::Counts {
        allocs: 0,
        frees: 0,
    };
    for b in 0..BLOCKS {
        for i in [2 * b, 2 * b + 1] {
            let batch = gesture(producer.batch(), i);
            producer.push(batch).unwrap();
        }
        let c = measure(|| {
            drain_control(&mut control, &mut slot, &diagnostics);
            slot.fill(&mut out);
        });
        total.allocs += c.allocs;
        total.frees += c.frees;
    }
    assert_eq!(
        (total.allocs, total.frees),
        (0, 0),
        "draining {} batches touched the heap on the render thread",
        2 * BLOCKS
    );
    let _ = producer.batch();
    assert_eq!(
        producer.outstanding(),
        0,
        "every batch went back to its producer"
    );
}

/// Batches a `send` stamped with the Plan it compiled for: one naming the live Plan is queued, one
/// naming another is dropped whole and marked, and neither touches the heap on the render thread.
#[test]
fn stamped_and_stale_batches_neither_allocate_nor_free_on_the_render_thread() {
    use reuben_api::engine::PreparedAgainst;
    use std::sync::Arc;

    let cfg = AudioConfig::new(48_000.0, 64);
    let (mut coordinator, side, _) =
        reuben_api::engine::install_initial(DOC, no_store(), cfg).expect("install");
    let live = PreparedAgainst::new(Arc::clone(side.engine.prepared_routes()));
    let (_other, other_side, _) =
        reuben_api::engine::install_initial(DOC, no_store(), cfg).expect("install");
    let stale = PreparedAgainst::new(Arc::clone(other_side.engine.prepared_routes()));
    let mut slot = RenderSlot::new(side);
    let mut out = vec![0.0f32; cfg.block_size * slot.channels()];
    let diagnostics = Diagnostics::new();
    let (mut producer, ring) = ring_pair(Diagnostics::new());
    let mut control = ControlDrain::new(vec![ring]);
    let stamped = |batch: ControlBatch, i: usize, against: &PreparedAgainst| {
        let mut batch = gesture(batch, i);
        batch[0].against = Some(against.clone());
        batch
    };

    for i in 0..4 {
        let batch = stamped(producer.batch(), i, &live);
        producer.push(batch).unwrap();
        drain_control(&mut control, &mut slot, &diagnostics);
        slot.fill(&mut out);
    }
    assert_counter_is_live();
    let mut total = rt_alloc::Counts {
        allocs: 0,
        frees: 0,
    };
    const BLOCKS: usize = 20;
    for b in 0..BLOCKS {
        let batch = stamped(producer.batch(), 2 * b, &live);
        producer.push(batch).unwrap();
        let batch = stamped(producer.batch(), 2 * b + 1, &stale);
        producer.push(batch).unwrap();
        let c = measure(|| {
            drain_control(&mut control, &mut slot, &diagnostics);
            slot.fill(&mut out);
        });
        total.allocs += c.allocs;
        total.frees += c.frees;
    }
    assert_eq!((total.allocs, total.frees), (0, 0));
    assert_eq!(
        diagnostics.snapshot().stale_sends_dropped,
        (3 * BLOCKS) as u64,
        "every message of every stale batch"
    );
    assert_eq!(
        producer.take_dropped(),
        vec!["/abs/x", "/filt/cutoff", "/filt/mode"]
    );

    // A swap committed: the drain holds what it pops, on the render thread, without the heap.
    assert!(coordinator.swap_document(DOC).report.ok);
    let batch = stamped(producer.batch(), 0, &live);
    producer.push(batch).unwrap();
    let c = measure(|| drain_control(&mut control, &mut slot, &diagnostics));
    assert_eq!((c.allocs, c.frees), (0, 0), "holding a batch");
    assert!(producer.take_dropped().is_empty());
    assert_eq!(producer.outstanding(), 1, "held, not handed back");
}

/// Two `abs` nodes, the second's input fed by the first, so a message to it is counted as a
/// wired write once it is queued and routed: the proof a batch reached the Engine.
const WIRED: &str = r#"{ "format_version": 3, "instrument": "wired",
    "interface": { "outputs": { "out": { "from": "/osc.audio" } } },
    "nodes": [
      { "type": "oscillator", "address": "/osc" },
      { "type": "abs_i32_value", "address": "/abs" },
      { "type": "abs_i32_value", "address": "/fed", "inputs": { "x": { "from": "/abs.out" } } } ] }"#;

/// While a swap is committed and not yet installed, the real drain leaves each batch on its ring,
/// queued nowhere; once the swap installs, the next drain queues it on the installed Engine.
#[test]
fn a_batch_drained_while_a_swap_installs_waits_then_lands_on_the_installed_engine() {
    let cfg = AudioConfig::new(48_000.0, 64);
    let (mut coordinator, side, _) =
        reuben_api::engine::install_initial(WIRED, no_store(), cfg).expect("install");
    let mut slot = RenderSlot::new(side);
    let mut out = vec![0.0f32; cfg.block_size * slot.channels()];
    let diagnostics = Diagnostics::new();
    let (mut producer, ring) = ring_pair(Diagnostics::new());
    let mut control = ControlDrain::new(vec![ring]);
    slot.fill(&mut out);

    assert!(coordinator.swap_document(WIRED).report.ok);
    let mut batch = producer.batch();
    batch.push(OscIn::new("/fed/x".to_string(), vec![Arg::I32(3)]));
    producer.push(batch).unwrap();

    let mut held_until = None;
    for block in 0..64 {
        let waiting = slot.install_pending();
        drain_control(&mut control, &mut slot, &diagnostics);
        slot.fill(&mut out);
        let routed = slot.take_dropped_wired_writes();
        if waiting {
            assert_eq!(routed, 0, "block {block}: queued while the swap installed");
            let _ = producer.batch();
            assert_eq!(producer.outstanding(), 1, "block {block}: held on its ring");
        } else if routed == 1 {
            held_until = Some(block);
            break;
        }
    }
    let block = held_until.expect("the held batch reached the installed Engine");
    assert!(block > 0, "it waited for the install");
    let _ = producer.batch();
    assert_eq!(producer.outstanding(), 0, "handed back once queued");
    assert_eq!(diagnostics.snapshot().stale_sends_dropped, 0);
}

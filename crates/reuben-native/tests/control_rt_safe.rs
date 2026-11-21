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
        OscIn {
            address: "/filt/cutoff".to_string(),
            args: vec![Arg::F32(200.0 + i as f32)],
        },
        OscIn {
            address: "/abs/x".to_string(),
            args: vec![Arg::I32(i as i32)],
        },
        OscIn {
            address: "/filt/mode".to_string(),
            args: vec![Arg::Str(
                if i.is_multiple_of(2) { "Hp" } else { "Bp" }.into(),
            )],
        },
    ]);
    batch
}

#[test]
fn draining_control_neither_allocates_nor_frees_on_the_render_thread() {
    let cfg = AudioConfig::new(48_000.0, 64);
    let (_coordinator, side, _) =
        reuben_api::engine::install_initial(DOC, no_store(), cfg).expect("install");
    let mut slot = RenderSlot::new(side);
    let mut out = vec![0.0f32; cfg.block_size * slot.channels()];
    let (mut producer, ring) = ring_pair(Diagnostics::new());
    let mut control = ControlDrain::new(vec![ring]);

    for i in 0..8 {
        let batch = gesture(producer.batch(), i);
        producer.push(batch).unwrap();
        drain_control(&mut control, &mut slot);
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
            drain_control(&mut control, &mut slot);
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

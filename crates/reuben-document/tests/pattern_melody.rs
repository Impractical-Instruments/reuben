//! The `pattern-melody` library instrument end to end: Clock → `pattern` → Voicer opens playing
//! its document's default pattern, and a pattern sent to its `str` pipe, prepared as the engine
//! channel's `send` prepares it, takes over at the next cycle.

use std::sync::Arc;

use reuben_core::message::Arg;
use reuben_core::resources::SampleBuffer;
use reuben_core::{AudioConfig, Engine, Plan, Registry};
use reuben_document::load_instrument;
use reuben_document::resources::{ResolveError, ResourceResolver};

const DOC: &str = include_str!("../../../instruments/pattern-melody.json");
const BLOCK: usize = 128;

/// Reads the voice patch from the repo's `instruments/`.
struct Library;

impl ResourceResolver for Library {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        let path = format!("{}/../../instruments/{source}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).map_err(|e| ResolveError::NotFound(format!("{path}: {e}")))
    }
}

fn engine() -> Engine {
    let graph = load_instrument(DOC, &Registry::builtin(), &Library)
        .expect("the library instrument loads")
        .graph;
    Engine::new(Plan::instantiate(graph, AudioConfig::new(48_000.0, BLOCK)).expect("plans"))
}

/// Peak level of each `blocks`-block window over `windows` windows.
fn peaks(e: &mut Engine, windows: usize, blocks: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; BLOCK * e.channels()];
    (0..windows)
        .map(|_| {
            let mut peak = 0.0f32;
            for _ in 0..blocks {
                e.fill(&mut out);
                peak = out.iter().fold(peak, |p, s| p.max(s.abs()));
            }
            peak
        })
        .collect()
}

#[test]
fn it_opens_audible() {
    let mut e = engine();
    // Two seconds, in quarter-second windows: the default pattern sounds throughout its first
    // cycle and a half.
    let got = peaks(&mut e, 8, 94);
    assert!(
        got.iter().all(|&p| p > 0.01),
        "every window sounds: {got:?}"
    );
}

#[test]
fn a_prepared_send_to_the_pattern_pipe_takes_over_at_the_next_cycle() {
    let mut e = engine();
    let route = e
        .prepared_routes()
        .get("/pattern/in")
        .expect("a send to the `pattern` pipe is prepared");
    let silence = route
        .preparer
        .prepare(&Arc::from("~"))
        .expect("a rest compiles");
    // 120 BPM over 128-frame blocks: a cycle of four beats is 750 blocks.
    e.queue_osc("/tempo/in", &[Arg::F32(120.0)]);
    let mut out = vec![0.0f32; BLOCK * e.channels()];
    for _ in 0..100 {
        e.fill(&mut out);
    }
    e.queue_osc("/pattern/in", &[Arg::Prepared(silence)]);
    // The rest of cycle 0 still plays the default; cycle 1 is silent once the last note's
    // release tail has died away.
    let before = peaks(&mut e, 1, 500);
    let _boundary_and_tail = peaks(&mut e, 1, 300);
    let after = peaks(&mut e, 1, 500);
    assert!(
        before[0] > 0.01,
        "the old pattern plays out its cycle: {before:?}"
    );
    assert!(after[0] < 1e-4, "the new pattern took over: {after:?}");
}

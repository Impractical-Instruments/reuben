//! The `pattern-lab` library instrument end to end: it opens audible, and a pattern sent to each
//! of its three `str` pipes, prepared as the engine channel's `send` prepares it, takes over.

use std::sync::Arc;

use reuben_core::message::Arg;
use reuben_core::resources::SampleBuffer;
use reuben_core::{AudioConfig, Engine, Plan, Registry};
use reuben_document::load_instrument;
use reuben_document::resources::{ResolveError, ResourceResolver};

const DOC: &str = include_str!("../../../instruments/pattern-lab.json");
const BLOCK: usize = 128;
/// At 240 BPM a 4-beat cycle is 1 s, 375 blocks.
const CYCLE_BLOCKS: usize = 375;

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
    let mut e =
        Engine::new(Plan::instantiate(graph, AudioConfig::new(48_000.0, BLOCK)).expect("plans"));
    e.queue_osc("/tempo/in", &[Arg::F32(240.0)]);
    e
}

fn send(e: &mut Engine, pipe: &str, text: &str) {
    let route = e
        .prepared_routes()
        .get(pipe)
        .unwrap_or_else(|| panic!("a send to {pipe} is prepared"));
    let payload = route
        .preparer
        .prepare(&Arc::from(text))
        .unwrap_or_else(|err| panic!("{text:?} compiles: {err:?}"));
    e.queue_osc(pipe, &[Arg::Prepared(payload)]);
}

/// The first channel of `blocks` blocks.
fn render(e: &mut Engine, blocks: usize) -> Vec<f32> {
    let channels = e.channels();
    let mut out = vec![0.0f32; BLOCK * channels];
    let mut all = Vec::new();
    for _ in 0..blocks {
        e.fill(&mut out);
        all.extend(out.chunks(channels).map(|f| f[0]));
    }
    all
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
}

fn crossings(x: &[f32]) -> usize {
    x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
}

/// Send `pipe` its text, play past the next cycle boundary, then render a window to measure.
fn after_send(e: &mut Engine, pipe: &str, text: &str) -> Vec<f32> {
    send(e, pipe, text);
    render(e, 2 * CYCLE_BLOCKS);
    render(e, 100)
}

#[test]
fn it_opens_audible() {
    let mut e = engine();
    let got = render(&mut e, 200);
    assert!(rms(&got) > 0.01, "rms {}", rms(&got));
}

#[test]
fn a_prepared_send_to_the_drone_pipe_changes_its_frequency() {
    let mut e = engine();
    send(&mut e, "/notes/in", "~");
    let low = after_send(&mut e, "/drone/in", "200");
    let high = after_send(&mut e, "/drone/in", "800");
    let ratio = crossings(&high) as f32 / crossings(&low) as f32;
    assert!(
        (3.5..4.5).contains(&ratio),
        "800 Hz crosses 4x as often: {ratio}"
    );
}

#[test]
fn a_prepared_send_to_the_notes_pipe_takes_over() {
    let mut e = engine();
    send(&mut e, "/notes/in", "~");
    send(&mut e, "/cutoff/in", "8000");
    let silent = after_send(&mut e, "/drone/in", "<65.4>");
    let played = after_send(&mut e, "/notes/in", "c3*4");
    assert!(
        rms(&played) > 1.2 * rms(&silent),
        "{} vs {}",
        rms(&played),
        rms(&silent)
    );
}

#[test]
fn a_prepared_send_to_the_cutoff_pipe_changes_the_filter() {
    let mut e = engine();
    send(&mut e, "/notes/in", "c3*4");
    let dull = after_send(&mut e, "/cutoff/in", "60");
    let bright = after_send(&mut e, "/cutoff/in", "8000");
    assert!(
        rms(&bright) > 1.2 * rms(&dull),
        "{} vs {}",
        rms(&bright),
        rms(&dull)
    );
}

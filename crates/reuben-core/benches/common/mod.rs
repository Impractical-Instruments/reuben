//! Shared bench fixtures and the one fixed, deterministic workload.
//!
//! Both bench harnesses — `macro_criterion` (wall-clock, local) and `macro_iai`
//! (instruction-count, CI gate) — drive the *same* `BenchState` so the two layers
//! measure identical work. The workload touches no clock and no RNG, so iai
//! instruction counts are byte-stable across runs (ADR-0019).
//!
//! Each bench binary compiles this module independently and uses only a subset of
//! it, so `dead_code` is expected.
#![allow(dead_code)]

use reuben_core::{load, Arg, AudioConfig, Message, Plan, Registry, Renderer, SerialExecutor};

/// Real shipped sample rate.
pub const SAMPLE_RATE: f32 = 48_000.0;
/// Real shipped default block size.
pub const BLOCK_SIZE: usize = 128;
/// Blocks per bench iteration. `375 * 128 == 48_000` == exactly 1 s of audio.
pub const BLOCKS: usize = 375;
/// Samples rendered per iteration — criterion throughput unit (`/ SAMPLE_RATE` == ×realtime).
pub const TOTAL_SAMPLES: u64 = (BLOCKS * BLOCK_SIZE) as u64;

/// Four-note chord (C E G C') — loads 4 of the voicer's 8 voices.
const CHORD: [f32; 4] = [60.0, 64.0, 67.0, 72.0];
/// Note-off at 0.5 s — exercises gate-on, sustain, *and* the release tail.
const NOTE_OFF_SAMPLE: usize = SAMPLE_RATE as usize / 2;

/// The benched instruments, each a real shipped JSON. Curated to span the heavy
/// operator families with no redundancy: reverb, delay, the modulation stack, and
/// the sampler/sequencer (non-oscillator) path (ADR-0019).
const FIXTURES: &[(&str, &str)] = &[
    (
        "reverb",
        include_str!("../../../../instruments/reverb.json"),
    ),
    ("echo", include_str!("../../../../instruments/echo.json")),
    (
        "auto-filter",
        include_str!("../../../../instruments/auto-filter.json"),
    ),
    (
        "sampler-arp",
        include_str!("../../../../instruments/sampler-arp.json"),
    ),
];

/// Names of the benched fixtures, for harnesses that iterate (criterion).
pub const FIXTURE_NAMES: &[&str] = &["reverb", "echo", "auto-filter", "sampler-arp"];

fn fixture_json(name: &str) -> &'static str {
    FIXTURES
        .iter()
        .find(|(n, _)| *n == name)
        .unwrap_or_else(|| panic!("unknown bench fixture {name:?}"))
        .1
}

/// The fixed messages for block `b`, with frames *relative to the block start*
/// (the contract `render_block` expects). Note-on at frame 0; note-off at 0.5 s.
fn block_messages(b: usize) -> Vec<Message> {
    let mut msgs = Vec::new();
    if b == 0 {
        for &m in &CHORD {
            msgs.push(Message::new(
                "/voicer/note",
                [Arg::Float(m), Arg::Float(1.0)],
                0,
            ));
        }
    }
    let off_block = NOTE_OFF_SAMPLE / BLOCK_SIZE;
    let off_frame = NOTE_OFF_SAMPLE % BLOCK_SIZE;
    if b == off_block {
        for &m in &CHORD {
            msgs.push(Message::new(
                "/voicer/note",
                [Arg::Float(m), Arg::Float(0.0)],
                off_frame,
            ));
        }
    }
    msgs
}

/// A fully-prepared, ready-to-render bench. Built by [`build_state`] *outside* the
/// measured region; only [`BenchState::render`] is timed.
pub struct BenchState {
    plan: Plan,
    renderer: Renderer<SerialExecutor>,
    /// Per-block message schedule, precomputed so the timed loop allocates nothing.
    schedule: Vec<Vec<Message>>,
    out: Vec<f32>,
}

/// Load `name`, instantiate its plan, prime the renderer, and precompute the
/// message schedule. Setup only — never call this inside a measured region.
pub fn build_state(name: &str) -> BenchState {
    let graph = load(fixture_json(name), &Registry::builtin()).expect("fixture loads");
    let plan = Plan::instantiate(graph, AudioConfig::new(SAMPLE_RATE, BLOCK_SIZE))
        .expect("fixture instantiates");
    let renderer = Renderer::new(&plan);
    let schedule = (0..BLOCKS).map(block_messages).collect();
    BenchState {
        plan,
        renderer,
        schedule,
        out: vec![0.0; BLOCK_SIZE],
    }
}

impl BenchState {
    /// Render the full fixed workload. Accumulates one sample per block so the
    /// optimizer cannot elide the work; the sum is the bench's return value.
    pub fn render(mut self) -> f32 {
        let mut acc = 0.0;
        for block in &self.schedule {
            self.renderer
                .render_block(&mut self.plan, block, &mut self.out);
            acc += self.out[0];
        }
        acc
    }
}

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

use reuben_core::resources::{ResolveError, ResourceResolver, SampleBuffer};
use reuben_core::vocab::pitch::{Note, Pitch};
use reuben_core::{
    load_instrument, AudioConfig, Message, Plan, Registry, Renderer, SerialExecutor,
};

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

/// A benched instrument: its name, its shipped JSON, and the OSC address external note-on/off
/// Messages enter on. The address differs per graph — most take notes straight at the Voicer
/// (`/voicer/notes`), but the tonal `autotune` graph feeds its quantizer first (`/snap/notes`),
/// which resolves and forwards degrees to the Voicer.
struct Fixture {
    name: &'static str,
    json: &'static str,
    note_addr: &'static str,
}

/// The benched instruments, each a real shipped JSON. Curated to span the heavy operator families
/// with no redundancy: reverb (comb/allpass banks), echo (delay feedback), auto-filter (the
/// lfo + m2s + math modulation stack), sampler-arp (sample + clock + sequencer, the non-oscillator
/// path), and autotune — the tonal-context path (harmony → snap → voicer), which exercises the
/// `hz`/`snap`/`chord_tone` resolver and context-driven block-slicing nothing else here touches
/// (#30, ADR-0013/0019).
const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "reverb",
        json: include_str!("../../../../instruments/reverb.json"),
        note_addr: "/voicer/notes",
    },
    Fixture {
        name: "echo",
        json: include_str!("../../../../instruments/echo.json"),
        note_addr: "/voicer/notes",
    },
    Fixture {
        name: "auto-filter",
        json: include_str!("../../../../instruments/auto-filter.json"),
        note_addr: "/voicer/notes",
    },
    Fixture {
        name: "sampler-arp",
        json: include_str!("../../../../instruments/sampler-arp.json"),
        note_addr: "/voicer/notes",
    },
    Fixture {
        name: "autotune",
        json: include_str!("../../../../instruments/autotune.json"),
        note_addr: "/snap/notes",
    },
];

/// Names of the benched fixtures, for harnesses that iterate (criterion).
pub const FIXTURE_NAMES: &[&str] = &["reverb", "echo", "auto-filter", "sampler-arp", "autotune"];

fn fixture(name: &str) -> &'static Fixture {
    FIXTURES
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("unknown bench fixture {name:?}"))
}

/// The fixed messages for block `b`, with frames *relative to the block start*
/// (the contract `render_block` expects). Note-on at frame 0; note-off at 0.5 s.
/// `note_addr` is the fixture's note entry point (e.g. `/voicer/notes`, or `/snap/notes` for the
/// tonal graph), so the same chord schedule drives every graph at its own front door.
fn block_messages(b: usize, note_addr: &str) -> Vec<Message> {
    let mut msgs = Vec::new();
    if b == 0 {
        for &m in &CHORD {
            msgs.push(Message::new(
                note_addr,
                Note::new(Pitch::Absolute(m), 1.0),
                0,
            ));
        }
    }
    let off_block = NOTE_OFF_SAMPLE / BLOCK_SIZE;
    let off_frame = NOTE_OFF_SAMPLE % BLOCK_SIZE;
    if b == off_block {
        for &m in &CHORD {
            msgs.push(Message::new(
                note_addr,
                Note::new(Pitch::Absolute(m), 0.0),
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

/// Resolves a fixture's resources from the repo `instruments/` dir, so voices *bind* and the bench
/// renders the real hosted-voice workload (#102) — not the degraded empty-voicer path `load()` gives.
/// `resolve_text` reads a voice patch's JSON (ADR-0032 §2); `resolve` decodes a WAV (mirrors
/// `reuben-native`'s `FsResolver`, using the `hound` dev-dependency) so `sampler-arp`'s sample player
/// reads real data instead of idling on an empty buffer. Setup-only IO — the timed `render` reads
/// none of it — and the decoded bytes are fixed, so iai instruction counts stay byte-stable (ADR-0019).
struct InstrumentsDir;
impl ResourceResolver for InstrumentsDir {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        let path = format!("{}/../../instruments/{source}", env!("CARGO_MANIFEST_DIR"));
        let mut reader = hound::WavReader::open(&path)
            .map_err(|e| ResolveError::NotFound(format!("{path}: {e}")))?;
        let spec = reader.spec();
        let channels = spec.channels as usize;
        if channels == 0 {
            return Err(ResolveError::Decode("zero channels".to_string()));
        }
        let mut planar: Vec<Vec<f32>> = vec![Vec::new(); channels];
        match spec.sample_format {
            hound::SampleFormat::Float => {
                for (i, s) in reader.samples::<f32>().enumerate() {
                    planar[i % channels].push(s.map_err(|e| ResolveError::Decode(e.to_string()))?);
                }
            }
            hound::SampleFormat::Int => {
                let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
                for (i, s) in reader.samples::<i32>().enumerate() {
                    let v = s.map_err(|e| ResolveError::Decode(e.to_string()))?;
                    planar[i % channels].push(v as f32 / scale);
                }
            }
        }
        Ok(SampleBuffer::new(planar, spec.sample_rate as f32))
    }

    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        let path = format!("{}/../../instruments/{source}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).map_err(|e| ResolveError::NotFound(format!("{path}: {e}")))
    }

    /// Per-document rebase, like `reuben-native`'s `FsResolver`: identity keys stay
    /// `instruments/`-relative, and a nested patch's own references (e.g. a voice's
    /// `../samples/blip.wav`) resolve next to *it*, lexically normalized.
    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        let mut out: Vec<&str> = referrer
            .map(|r| {
                let mut parts: Vec<&str> = r.split('/').collect();
                parts.pop(); // drop the referrer's file name, keep its directory
                parts
            })
            .unwrap_or_default();
        for part in source.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    out.pop();
                }
                p => out.push(p),
            }
        }
        out.join("/")
    }
}

/// Load `name`, instantiate its plan, prime the renderer, and precompute the
/// message schedule. Setup only — never call this inside a measured region.
pub fn build_state(name: &str) -> BenchState {
    let fx = fixture(name);
    let loaded =
        load_instrument(fx.json, &Registry::builtin(), &InstrumentsDir).expect("fixture loads");
    // The bug this fixes (#102) was invisible because nothing checked the workload was real: a
    // resource that fails to resolve degrades to silence + a warning, not an error. Treat any
    // warning as fatal so the bench can't silently fall back to the empty workload again.
    assert!(
        loaded.warnings.is_empty(),
        "fixture {name:?} loaded with resource warnings (bench would render a degraded workload): {:?}",
        loaded.warnings,
    );
    let plan = Plan::instantiate(loaded.graph, AudioConfig::new(SAMPLE_RATE, BLOCK_SIZE))
        .expect("fixture instantiates");
    let renderer = Renderer::new(&plan);
    let schedule = (0..BLOCKS)
        .map(|b| block_messages(b, fx.note_addr))
        .collect();
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

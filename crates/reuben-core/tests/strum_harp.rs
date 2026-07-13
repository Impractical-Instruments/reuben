//! Integration: the V1.3 Strum harp end-to-end (ADR-0022 §3 / ADR-0032) — dragging the strum bar
//! across the string bands plucks degrees, which the Voicer resolves through the tonal context and
//! hands to 8 hosted `resonator` voices.
//!
//! The assertion that earns its keep here is the **level**. The resonator's modal bank has two
//! excitation paths that need different input gains (a sustained tone is normalized against the
//! bank's resonant gain, a struck ping against its impulse response). Sharing one gain across both
//! made every pluck come out scaled by `(1 - r²)` — about 1e-4 at the default ring time — so the
//! harp loaded, validated, and rendered a perfectly correct signal roughly 43 dB below anything you
//! could hear. Nothing in the graph was wrong, so no graph-level test could see it: only rendering
//! the shipped instrument and looking at the actual amplitude catches it.

use reuben_core::message::{Arg, Message};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::{ResolveError, ResourceResolver, SampleBuffer};
use reuben_core::{load_instrument, AudioConfig, Registry};

const STRUM_HARP: &str = include_str!("../../../instruments/strum-harp.json");

const CFG: AudioConfig = AudioConfig {
    sample_rate: 48_000.0,
    block_size: 256,
    channels: AudioConfig::MIN_CHANNELS,
    input_channels: 0,
};

/// Reads the harp's hosted voice sub-patch out of the repo's `instruments/` dir.
struct InstrumentDir;
impl ResourceResolver for InstrumentDir {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        let path = format!("{}/../../instruments/{source}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).map_err(|e| ResolveError::NotFound(format!("{path}: {e}")))
    }
}

fn harp() -> Plan {
    let graph = load_instrument(STRUM_HARP, &Registry::builtin(), &InstrumentDir)
        .expect("load strum-harp instrument")
        .graph;
    Plan::instantiate(graph, CFG).expect("instantiate")
}

/// Drag the strum bar from 0 to 1 over `blocks` blocks (crossing every string band, which plucks
/// each degree in turn), then let the harp ring for `tail` blocks. Returns the peak output.
fn strum_and_ring(blocks: usize, tail: usize) -> f32 {
    let mut plan = harp();
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; CFG.block_size];
    let mut peak = 0.0f32;

    for i in 0..blocks {
        let pos = i as f32 / blocks as f32;
        let drag = Message::new("/strum/in", Arg::F32(pos), 0);
        r.render_block(&mut plan, std::slice::from_ref(&drag), &mut buf);
        peak = peak.max(buf.iter().fold(0.0, |m, &s| m.max(s.abs())));
    }
    for _ in 0..tail {
        r.render_block(&mut plan, &[], &mut buf);
        peak = peak.max(buf.iter().fold(0.0, |m, &s| m.max(s.abs())));
    }
    peak
}

#[test]
fn a_glissando_is_loud_enough_to_hear_and_does_not_clip() {
    // ~1 s drag across all 8 strings, then ~1 s of ring-out. A harp that plays at a sane level lands
    // in a broad musical band: loud enough to be a voice, quiet enough that 8 summed resonators
    // don't run off the clip ceiling (the `/vtrim` -> `/trim` headroom stage).
    let peak = strum_and_ring(180, 180);
    assert!(
        peak > 0.1,
        "strum-harp is inaudibly quiet (peak {peak}) — the resonator's ping gain has regressed"
    );
    assert!(
        peak <= 1.0,
        "strum-harp clips a full glissando (peak {peak}) — needs more headroom trim"
    );
}

#[test]
fn every_string_speaks_at_a_comparable_level() {
    // Pluck each string on its own and compare peaks. A harp whose glissando fades out as it climbs
    // is the signature of a pitch-dependent ping gain; the resonator's `sin(w)` mode normalization
    // plus its `1/√freq` contact time is what holds the strings even.
    let strings = 8i32;
    let band_center = |b: i32| (b as f32 + 0.5) / strings as f32;
    let mut peaks = Vec::new();

    for s in 0..strings {
        let mut plan = harp();
        let mut r = Renderer::new(&plan);
        let mut buf = vec![0.0f32; CFG.block_size];

        // The strum op latches the first position it sees without plucking, then plucks every band
        // it crosses. So park on a neighbouring band, then cross exactly one boundary onto string
        // `s`: one pluck, one string, nothing else ringing.
        let park = if s == 0 { 1 } else { s - 1 };
        for b in [park, s] {
            let m = Message::new("/strum/in", Arg::F32(band_center(b)), 0);
            r.render_block(&mut plan, std::slice::from_ref(&m), &mut buf);
        }

        let mut peak = 0.0f32;
        for _ in 0..120 {
            r.render_block(&mut plan, &[], &mut buf);
            peak = peak.max(buf.iter().fold(0.0, |m, &s| m.max(s.abs())));
        }
        peaks.push(peak);
    }

    let lo = peaks.iter().copied().fold(f32::MAX, f32::min);
    let hi = peaks.iter().copied().fold(0.0f32, f32::max);
    assert!(lo > 0.05, "a string was inaudible: {peaks:?}");
    assert!(
        hi < lo * 3.0,
        "strings must speak at a comparable level (within ~10 dB): {peaks:?}"
    );
}

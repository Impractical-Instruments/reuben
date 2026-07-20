//! Integration: voice-liveness — a voice patch's `interface` outputs are read by
//! the host the same way an operator reads a port. `audio` (Signal) resolves to an arena buffer;
//! `active` (Value) is captured into [`Plan::captured`] each block (held ZOH). This proves the
//! capture seam end-to-end on a real, shipped voice patch (`default-voice`), driven standalone.

use reuben_core::message::{Arg, Message};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::{ResolveError, ResourceResolver, SampleBuffer};
use reuben_core::{load_instrument, AudioConfig, Registry};

const DEFAULT_VOICE: &str = include_str!("../../../instruments/voices/default-voice.json");

const CFG: AudioConfig = AudioConfig {
    sample_rate: 48_000.0,
    block_size: 128,
    channels: AudioConfig::MIN_CHANNELS,
    input_channels: 0,
};

/// `default-voice` pulls no resources; everything fails to resolve.
struct NullResolver;
impl ResourceResolver for NullResolver {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
}

fn voice_plan() -> Plan {
    let graph = load_instrument(DEFAULT_VOICE, &Registry::builtin(), &NullResolver)
        .expect("load default-voice")
        .graph;
    Plan::instantiate(graph, CFG).expect("instantiate")
}

fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0, |m, s| m.max(s.abs()))
}

#[test]
fn interface_outputs_resolve_to_a_signal_buffer_and_a_captured_value() {
    let plan = voice_plan();
    assert!(
        plan.interface_signal_buf("audio").is_some(),
        "audio interface output is a Signal arena buffer"
    );
    assert!(
        plan.interface_value_slot("active").is_some(),
        "active interface output is a captured Value"
    );
    // Exactly one Value interface output (`active`) → one captured slot, seeded 0.
    assert_eq!(plan.captured.len(), 1);
    assert_eq!(plan.captured[0], 0.0);
}

#[test]
fn active_is_captured_high_while_gated_and_returns_to_zero_after_release() {
    let mut plan = voice_plan();
    let active = plan.interface_value_slot("active").expect("active slot");
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; CFG.block_size];

    // Before any gate: idle, captured active is 0 and the voice is silent.
    r.render_block(&mut plan, &[], &mut buf);
    assert_eq!(plan.captured[active], 0.0, "idle voice is inactive");
    assert_eq!(peak(&buf), 0.0, "idle voice is silent");

    // Gate on (held Value to /env.gate): active latches high and audio sounds.
    let gate_on = Message::new("/env/gate", Arg::F32(1.0), 0);
    r.render_block(&mut plan, &[gate_on], &mut buf);
    assert_eq!(plan.captured[active], 1.0, "active high through the note");
    assert!(peak(&buf) > 0.0, "gated voice sounds");

    // Hold a few blocks: stays active (no re-emit needed — held ZOH).
    for _ in 0..4 {
        r.render_block(&mut plan, &[], &mut buf);
    }
    assert_eq!(plan.captured[active], 1.0, "held note stays active");

    // Gate off: active must remain high through the release tail, then fall to 0 once idle.
    let gate_off = Message::new("/env/gate", Arg::F32(0.0), 0);
    r.render_block(&mut plan, &[gate_off], &mut buf);

    let mut went_idle = false;
    for _ in 0..2000 {
        r.render_block(&mut plan, &[], &mut buf);
        if plan.captured[active] == 0.0 {
            went_idle = true;
            break;
        }
    }
    assert!(went_idle, "active returns to 0 after the release tail");
}

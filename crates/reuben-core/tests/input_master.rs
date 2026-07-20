//! The core input master (P3 — issue #180).
//!
//! Top-level signal input pipes with a `channel` binding read the **logical input master**:
//! the caller hands `render_block_multi` one buffer per logical input channel and each bound
//! pipe copies its channel — the dual of the output master. These tests pin the whole
//! contract through the real load → instantiate → render path:
//!
//! - a bound pipe carries the injected channel, sample-exact, across blocks;
//! - fan-out at the master (two pipes, one channel), like output broadcast;
//! - dark-degrade: an unsupplied channel falls back to the pipe's declared default (a
//!   bare pipe reads **zeros**) and stays message-drivable; a short buffer's tail reads zeros;
//! - determinism: offline render with injected input is **bit-reproducible**;
//! - inertness: nested (subpatch-inlined) and Voicer-hosted channel bindings never
//!   reach the input master — the parent/host edge feeds the pipe, unfed renders silence —
//!   with the load warnings that make each dark path honest.

use reuben_core::message::{Arg, Message};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::MemoryResolver;
use reuben_core::vocab::pitch::{Note, Pitch};
use reuben_core::{load_instrument, AudioConfig, LoadError, LoadWarning, Registry};

const BLOCK: usize = 128;

/// Deterministic, channel-distinct, block-varying test signal in `[-0.5, 0.5)` — a pure
/// function of `(channel, global frame)`, so every render of it is bit-identical.
fn test_input(channel: usize, global_frame: usize) -> f32 {
    ((global_frame * (channel + 3)) % 97) as f32 / 97.0 - 0.5
}

/// Load `json` (nested refs via `resolver`), instantiate, and render `blocks` blocks with
/// `test_input` injected on every logical input channel the plan derives. Returns
/// `(concatenated master channels, the plan's logical input width)`.
fn render_with_input(
    json: &str,
    resolver: &MemoryResolver,
    blocks: usize,
) -> (Vec<Vec<f32>>, usize) {
    let loaded = load_instrument(json, &Registry::builtin(), resolver).expect("load");
    let mut plan =
        Plan::instantiate(loaded.graph, AudioConfig::new(48_000.0, BLOCK)).expect("instantiate");
    let in_ch = plan.config.input_channels;
    let channels = plan.config.channels;
    let mut r = Renderer::new(&plan);
    let mut master: Vec<Vec<f32>> = (0..channels).map(|_| vec![0.0; BLOCK]).collect();
    let mut inputs: Vec<Vec<f32>> = (0..in_ch).map(|_| vec![0.0; BLOCK]).collect();
    let mut outbound = Vec::new();
    let mut all: Vec<Vec<f32>> = (0..channels).map(|_| Vec::new()).collect();
    for b in 0..blocks {
        for (c, chan) in inputs.iter_mut().enumerate() {
            for (f, s) in chan.iter_mut().enumerate() {
                *s = test_input(c, b * BLOCK + f);
            }
        }
        r.render_block_multi(&mut plan, &[], &inputs, &mut master, &mut outbound);
        for (chan, sink) in master.iter().zip(all.iter_mut()) {
            sink.extend_from_slice(chan);
        }
    }
    (all, in_ch)
}

// A one-pipe passthrough: logical input channel 0 straight to the master.
const MIC_THROUGH: &str = r#"{
  "format_version": 2,
  "instrument": "mic_through",
  "interface": {
    "inputs":  { "mic": { "type": "f32_buffer", "channel": 0 } },
    "outputs": { "main": { "from": "/out.audio" } }
  },
  "nodes": [
    { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/mic" } } }
  ]
}"#;

#[test]
fn bound_pipe_reads_its_logical_input_channel() {
    let none = MemoryResolver::new();
    let (out, in_ch) = render_with_input(MIC_THROUGH, &none, 3);
    assert_eq!(in_ch, 1, "max bound input channel + 1");
    assert_eq!(out.len(), 2, "output master keeps its stereo floor");
    for (g, s) in out[0].iter().enumerate() {
        assert_eq!(
            s.to_bits(),
            test_input(0, g).to_bits(),
            "sample {g}: the pipe must carry logical input channel 0 verbatim"
        );
    }
    // A broadcast output tap fans the same signal to every master channel.
    assert_eq!(out[0], out[1]);
}

#[test]
fn distinct_pipes_fan_out_one_channel_at_the_master() {
    // Two pipes may bind the same channel — fan-out at the master, like output
    // broadcast. Summing both must yield exactly 2x the injected signal (x + x is exact in f32).
    const FAN: &str = r#"{
      "format_version": 2,
      "instrument": "fan",
      "interface": {
        "inputs": {
          "mic_a": { "type": "f32_buffer", "channel": 0 },
          "mic_b": { "type": "f32_buffer", "channel": 0 }
        },
        "outputs": { "main": { "from": "/out.audio" } }
      },
      "nodes": [
        { "type": "add_f32_signal", "address": "/sum",
          "inputs": { "a": { "from": "/mic_a" }, "b": { "from": "/mic_b" } } },
        { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/sum" } } }
      ]
    }"#;
    let none = MemoryResolver::new();
    let (out, in_ch) = render_with_input(FAN, &none, 2);
    assert_eq!(in_ch, 1, "two pipes on one channel need only one channel");
    for (g, s) in out[0].iter().enumerate() {
        assert_eq!(
            s.to_bits(),
            (2.0 * test_input(0, g)).to_bits(),
            "sample {g}: both pipes must read the same channel"
        );
    }
}

#[test]
fn zero_input_is_silence_from_an_input_pipe() {
    // Dark-degrade: a caller that supplies no input at all — the `&[]` every
    // pre-input call site passes — reads zeros through every bound pipe.
    let none = MemoryResolver::new();
    let loaded = load_instrument(MIC_THROUGH, &Registry::builtin(), &none).expect("load");
    let mut plan =
        Plan::instantiate(loaded.graph, AudioConfig::new(48_000.0, BLOCK)).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut master: Vec<Vec<f32>> = vec![vec![1.0; BLOCK]; plan.config.channels];
    let mut outbound = Vec::new();
    for _ in 0..3 {
        r.render_block_multi(&mut plan, &[], &[], &mut master, &mut outbound);
        assert!(
            master.iter().all(|ch| ch.iter().all(|&s| s == 0.0)),
            "an unfed input pipe must render exact silence"
        );
    }
    // The mono convenience is a no-input path by contract: same silence.
    let mut mono = vec![1.0f32; BLOCK];
    r.render_block(&mut plan, &[], &mut mono);
    assert!(mono.iter().all(|&s| s == 0.0));
}

#[test]
fn unsupplied_channel_and_short_buffer_read_zeros() {
    // A pipe bound past what the caller supplies, and the tail of a short buffer, both read
    // zeros — missing reality degrades dark, never fatally and never stale.
    const MIC_CH1: &str = r#"{
      "format_version": 2,
      "instrument": "mic_ch1",
      "interface": {
        "inputs":  { "mic": { "type": "f32_buffer", "channel": 1 } },
        "outputs": { "main": { "from": "/out.audio" } }
      },
      "nodes": [
        { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/mic" } } }
      ]
    }"#;
    let none = MemoryResolver::new();
    let loaded = load_instrument(MIC_CH1, &Registry::builtin(), &none).expect("load");
    let mut plan =
        Plan::instantiate(loaded.graph, AudioConfig::new(48_000.0, BLOCK)).expect("instantiate");
    assert_eq!(plan.config.input_channels, 2, "channel 1 derives width 2");
    let mut r = Renderer::new(&plan);
    let mut master: Vec<Vec<f32>> = vec![vec![1.0; BLOCK]; plan.config.channels];
    let mut outbound = Vec::new();

    // Only channel 0 supplied; the pipe reads (missing) channel 1 -> silence.
    let only_ch0 = vec![vec![0.25f32; BLOCK]];
    r.render_block_multi(&mut plan, &[], &only_ch0, &mut master, &mut outbound);
    assert!(
        master[0].iter().all(|&s| s == 0.0),
        "missing channel reads zeros"
    );

    // Channel 1 supplied short (32 of 128 frames): the tail reads zeros — and stays zeros on
    // the next block (no stale carry-over from the previous full copy).
    let short = vec![vec![0.0f32; BLOCK], vec![0.5f32; 32]];
    r.render_block_multi(&mut plan, &[], &short, &mut master, &mut outbound);
    assert!(
        master[0][..32].iter().all(|&s| s == 0.5),
        "supplied head is carried"
    );
    assert!(
        master[0][32..].iter().all(|&s| s == 0.0),
        "short tail reads zeros"
    );
    r.render_block_multi(&mut plan, &[], &short, &mut master, &mut outbound);
    assert!(
        master[0][32..].iter().all(|&s| s == 0.0),
        "no stale data across blocks"
    );
}

#[test]
fn offline_render_with_injected_input_is_bit_reproducible() {
    // The determinism carve-out: live input is a sanctioned nondeterministic
    // boundary, but the offline path injects *known* buffers — so two renders of the same
    // graph with the same injected input are bit-identical. The delay gives the render state
    // that persists across blocks, so any nondeterminism would compound visibly.
    const WET: &str = r#"{
      "format_version": 2,
      "instrument": "wet_mic",
      "interface": {
        "inputs":  { "mic": { "type": "f32_buffer", "channel": 0 } },
        "outputs": { "main": { "from": "/out.audio" } }
      },
      "nodes": [
        { "type": "delay", "address": "/echo", "inputs": { "audio": { "from": "/mic" } } },
        { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/echo" } } }
      ]
    }"#;
    let none = MemoryResolver::new();
    let (a, _) = render_with_input(WET, &none, 8);
    let (b, _) = render_with_input(WET, &none, 8);
    assert!(
        a[0].iter().any(|s| s.abs() > 0.01),
        "injected input must actually sound"
    );
    for (ch, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        for (g, (p, q)) in x.iter().zip(y.iter()).enumerate() {
            assert_eq!(
                p.to_bits(),
                q.to_bits(),
                "channel {ch} sample {g}: injected-input render must be bit-reproducible"
            );
        }
    }
}

// A child whose pipe binds a channel — inert once nested/hosted.
const CHILD_BOUND: &str = r#"{
  "format_version": 2,
  "instrument": "gainer",
  "interface": {
    "inputs":  { "in": { "type": "f32_buffer", "channel": 5 } },
    "outputs": { "audio": { "from": "/g.audio" } }
  },
  "nodes": [
    { "type": "output", "address": "/g", "inputs": { "audio": { "from": "/in" } } }
  ]
}"#;

#[test]
fn nested_channel_binding_is_inert_parent_edge_feeds_the_pipe() {
    // The parent wires its own (channel-0-bound) pipe into the child's boundary. The child's
    // `channel: 5` must not leak: the input width derives from the parent's own pipes only,
    // and the nested pipe carries the parent edge, not logical channel 5.
    const PARENT_WIRED: &str = r#"{
      "format_version": 2,
      "instrument": "host",
      "resources": { "fx": "gainer.json" },
      "interface": {
        "inputs":  { "mic": { "type": "f32_buffer", "channel": 0 } },
        "outputs": { "main": { "from": "/out.audio" } }
      },
      "nodes": [
        { "type": "subpatch", "address": "/fx", "patch": "fx",
          "inputs": { "in": { "from": "/mic" } } },
        { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/fx.audio" } } }
      ]
    }"#;
    let mut resolver = MemoryResolver::new();
    resolver.insert_text("gainer.json", CHILD_BOUND);
    let (out, in_ch) = render_with_input(PARENT_WIRED, &resolver, 2);
    assert_eq!(
        in_ch, 1,
        "the child's channel 5 is inert — width derives from the parent's pipes alone"
    );
    for (g, s) in out[0].iter().enumerate() {
        assert_eq!(
            s.to_bits(),
            test_input(0, g).to_bits(),
            "sample {g}: the parent edge (channel 0) feeds the nested pipe"
        );
    }
}

#[test]
fn nested_channel_binding_unwired_renders_silence_with_a_warning() {
    // The host leaves the child's bound pipe unfed: no hardware fallback reach-through
    // (channel binding is rejected there) — silence, plus the UnwiredPipe warning.
    const PARENT_UNWIRED: &str = r#"{
      "format_version": 2,
      "instrument": "host",
      "resources": { "fx": "gainer.json" },
      "interface": { "outputs": { "main": { "from": "/out.audio" } } },
      "nodes": [
        { "type": "subpatch", "address": "/fx", "patch": "fx" },
        { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/fx.audio" } } }
      ]
    }"#;
    let mut resolver = MemoryResolver::new();
    resolver.insert_text("gainer.json", CHILD_BOUND);
    let loaded = load_instrument(PARENT_UNWIRED, &Registry::builtin(), &resolver).expect("load");
    assert!(
        loaded.warnings.iter().any(|w| matches!(
            w,
            LoadWarning::UnwiredPipe { node, name } if node == "/fx" && name == "in"
        )),
        "an unfed nested pipe warns: {:?}",
        loaded.warnings
    );
    let mut plan =
        Plan::instantiate(loaded.graph, AudioConfig::new(48_000.0, BLOCK)).expect("instantiate");
    assert_eq!(
        plan.config.input_channels, 0,
        "no top-level binding, no input master"
    );
    assert!(plan.input_taps.is_empty());
    // Even with six channels supplied, the nested binding must not reach the mic.
    let inputs: Vec<Vec<f32>> = vec![vec![0.5; BLOCK]; 6];
    let mut r = Renderer::new(&plan);
    let mut master: Vec<Vec<f32>> = vec![vec![1.0; BLOCK]; plan.config.channels];
    let mut outbound = Vec::new();
    r.render_block_multi(&mut plan, &[], &inputs, &mut master, &mut outbound);
    assert!(
        master[0].iter().all(|&s| s == 0.0),
        "an unwired nested binding renders silence, never hardware"
    );
}

#[test]
fn hosted_voice_channel_binding_is_inert_and_warns() {
    // A Voicer-hosted voice's channel binding is inert: hosted voice plans get
    // no input-master plumbing, so the voice's bound pipe renders silence even when the
    // caller supplies that channel — and the load says so.
    const HOST: &str = r#"{
      "format_version": 2,
      "instrument": "vhost",
      "resources": { "v": "vmic.json" },
      "interface": { "outputs": { "main": { "from": "/out.audio" } } },
      "nodes": [
        { "type": "voicer", "address": "/voicer", "voice": "v", "config": { "voices": 2 } },
        { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/voicer.audio" } } }
      ]
    }"#;
    const VOICE_MIC: &str = r#"{
      "format_version": 2,
      "instrument": "vmic",
      "interface": {
        "inputs":  { "mic": { "type": "f32_buffer", "channel": 0 } },
        "outputs": { "audio": { "from": "/o.audio" } }
      },
      "nodes": [
        { "type": "output", "address": "/o", "inputs": { "audio": { "from": "/mic" } } }
      ]
    }"#;
    let mut resolver = MemoryResolver::new();
    resolver.insert_text("vmic.json", VOICE_MIC);
    let loaded = load_instrument(HOST, &Registry::builtin(), &resolver).expect("load");
    let inert = loaded.warnings.iter().any(|w| {
        matches!(
            w,
            LoadWarning::Nested { node, warning }
                if node == "/voicer"
                    && matches!(warning.as_ref(),
                        LoadWarning::InertChannelBinding { name } if name == "mic")
        )
    });
    assert!(
        inert,
        "a hosted voice's channel binding warns inert: {:?}",
        loaded.warnings
    );

    let mut plan =
        Plan::instantiate(loaded.graph, AudioConfig::new(48_000.0, BLOCK)).expect("instantiate");
    assert_eq!(
        plan.config.input_channels, 0,
        "hosted bindings derive no top-level input width"
    );
    let inputs = vec![vec![0.5f32; BLOCK]];
    let mut r = Renderer::new(&plan);
    let mut master: Vec<Vec<f32>> = vec![vec![1.0; BLOCK]; plan.config.channels];
    let mut outbound = Vec::new();
    for _ in 0..3 {
        r.render_block_multi(&mut plan, &[], &inputs, &mut master, &mut outbound);
        assert!(
            master[0].iter().all(|&s| s == 0.0),
            "a hosted voice's bound pipe must not reach the input master"
        );
    }
}

#[test]
fn top_level_bare_pipe_without_a_channel_warns_unbound() {
    // issue #180: an unbound-but-declared bare signal pipe at top level renders
    // zeros — nothing can ever feed it — so the load says so. A channel-bound pipe, a
    // defaulted (control) signal pipe, and a Value pipe are all fine.
    let none = MemoryResolver::new();
    const UNBOUND: &str = r#"{
      "format_version": 2,
      "instrument": "u",
      "interface": {
        "inputs":  { "in": { "type": "f32_buffer" } },
        "outputs": { "main": { "from": "/out.audio" } }
      },
      "nodes": [
        { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/in" } } }
      ]
    }"#;
    let loaded = load_instrument(UNBOUND, &Registry::builtin(), &none).expect("load");
    assert!(
        loaded.warnings.iter().any(|w| matches!(
            w,
            LoadWarning::UnboundInputPipe { name } if name == "in"
        )),
        "expected the unbound-pipe warning: {:?}",
        loaded.warnings
    );

    // Bound, defaulted-control, and Value pipes stay quiet.
    const QUIET: &str = r#"{
      "format_version": 2,
      "instrument": "q",
      "interface": {
        "inputs": {
          "mic":  { "type": "f32_buffer", "channel": 0 },
          "tone": { "type": "f32_buffer", "default": 440, "min": 20, "max": 8000 },
          "mix":  { "type": "f32", "default": 0.5, "min": 0, "max": 1 }
        },
        "outputs": { "main": { "from": "/out.audio" } }
      },
      "nodes": [
        { "type": "oscillator", "address": "/osc", "inputs": { "freq": { "from": "/tone" } } },
        { "type": "add_f32_signal", "address": "/sum",
          "inputs": { "a": { "from": "/mic" }, "b": { "from": "/osc.audio" } } },
        { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/sum" } } }
      ]
    }"#;
    let loaded = load_instrument(QUIET, &Registry::builtin(), &none).expect("load");
    assert!(
        !loaded
            .warnings
            .iter()
            .any(|w| matches!(w, LoadWarning::UnboundInputPipe { .. })),
        "bound / defaulted / value pipes must not warn: {:?}",
        loaded.warnings
    );
}

#[test]
fn channel_bound_default_pipe_falls_back_to_its_default_unfed() {
    // #190 F1: `channel` + `default` on one pipe must not kill the knob.
    // Unfed, the declared default materializes (not zeros) and messages still sweep it;
    // fed, device audio wins for the block; unfed again, the held control resumes.
    const LEVEL: &str = r#"{
      "format_version": 2,
      "instrument": "lvl",
      "interface": {
        "inputs":  { "lvl": { "type": "f32_buffer", "channel": 0,
                              "default": 0.25, "min": 0, "max": 1 } },
        "outputs": { "main": { "from": "/out.audio" } }
      },
      "nodes": [
        { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/lvl" } } }
      ]
    }"#;
    let none = MemoryResolver::new();
    let loaded = load_instrument(LEVEL, &Registry::builtin(), &none).expect("load");
    assert!(
        loaded.warnings.is_empty(),
        "channel+default loads clean: {:?}",
        loaded.warnings
    );
    let mut plan =
        Plan::instantiate(loaded.graph, AudioConfig::new(48_000.0, BLOCK)).expect("instantiate");
    assert_eq!(plan.config.input_channels, 1);
    let mut r = Renderer::new(&plan);
    let mut master: Vec<Vec<f32>> = vec![vec![0.0; BLOCK]; plan.config.channels];
    let mut outbound = Vec::new();

    // Unfed: the declared default materializes — the control is at rest, not dead.
    r.render_block_multi(&mut plan, &[], &[], &mut master, &mut outbound);
    assert!(
        master[0].iter().all(|&s| s == 0.25),
        "unfed channel reads the declared default, not zeros: {:?}",
        &master[0][..4]
    );

    // Still unfed: a routed message sweeps the knob, exactly as without `channel`.
    let sweep = Message::new("/lvl/in", Arg::F32(0.75), 0);
    r.render_block_multi(&mut plan, &[sweep], &[], &mut master, &mut outbound);
    assert!(
        master[0].iter().all(|&s| s == 0.75),
        "an unfed channel-bound pipe stays message-drivable: {:?}",
        &master[0][..4]
    );

    // Fed: device audio wins for the block.
    let device = vec![vec![0.5f32; BLOCK]];
    r.render_block_multi(&mut plan, &[], &device, &mut master, &mut outbound);
    assert!(
        master[0].iter().all(|&s| s == 0.5),
        "a supplied channel drives the pipe: {:?}",
        &master[0][..4]
    );

    // Unfed again: the knob resumes at its last swept value (held ZOH).
    r.render_block_multi(&mut plan, &[], &[], &mut master, &mut outbound);
    assert!(
        master[0].iter().all(|&s| s == 0.75),
        "the held control resumes when the device stream goes away: {:?}",
        &master[0][..4]
    );
}

// A voice patch whose `freq` pipe carries a channel binding — the #190 F2 regression shape.
const BOUND_FREQ_VOICE: &str = r#"{
  "format_version": 2,
  "instrument": "bound_voice",
  "interface": {
    "inputs": {
      "freq": { "type": "f32_buffer", "channel": 0,
                "default": 440, "min": 20, "max": 20000 },
      "gate": { "type": "f32", "default": 0, "min": 0, "max": 1 }
    },
    "outputs": { "audio": { "from": "/vca.out" }, "active": { "from": "/env.active" } }
  },
  "nodes": [
    { "type": "oscillator", "address": "/osc", "inputs": { "freq": { "from": "/freq" } } },
    { "type": "envelope", "address": "/env", "inputs": { "gate": { "from": "/gate" } } },
    { "type": "mul_f32_signal", "address": "/vca",
      "inputs": { "a": { "from": "/osc" }, "b": { "from": "/env.cv" } } }
  ]
}"#;

const BOUND_FREQ_HOST: &str = r#"{
  "format_version": 2,
  "instrument": "vhost",
  "resources": { "v": "bound_voice.json" },
  "interface": { "outputs": { "main": { "from": "/out.audio" } } },
  "nodes": [
    { "type": "voicer", "address": "/voicer", "voice": "v", "config": { "voices": 2 } },
    { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/voicer.audio" } } }
  ]
}"#;

/// Render `blocks` blocks of BOUND_FREQ_HOST from a fresh plan: a note lands in block 0 and
/// every block is rendered with `inputs` as the logical input master.
fn render_bound_freq_host(blocks: usize, inputs: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let mut resolver = MemoryResolver::new();
    resolver.insert_text("bound_voice.json", BOUND_FREQ_VOICE);
    let loaded = load_instrument(BOUND_FREQ_HOST, &Registry::builtin(), &resolver).expect("load");
    let mut plan =
        Plan::instantiate(loaded.graph, AudioConfig::new(48_000.0, BLOCK)).expect("instantiate");
    assert_eq!(
        plan.config.input_channels, 0,
        "hosted bindings must derive no top-level input width"
    );
    let mut r = Renderer::new(&plan);
    let mut master: Vec<Vec<f32>> = vec![vec![0.0; BLOCK]; plan.config.channels];
    let mut outbound = Vec::new();
    let mut all: Vec<Vec<f32>> = vec![Vec::new(); plan.config.channels];
    for b in 0..blocks {
        let note_on = Message::new(
            "/voicer/notes",
            Arg::Note(Note::new(Pitch::Absolute(69.0), 1.0)),
            0,
        );
        let msgs = if b == 0 { vec![note_on] } else { Vec::new() };
        r.render_block_multi(&mut plan, &msgs, inputs, &mut master, &mut outbound);
        for (chan, sink) in master.iter().zip(all.iter_mut()) {
            sink.extend_from_slice(chan);
        }
    }
    all
}

#[test]
fn hosted_voice_with_channel_bound_freq_stays_message_fed() {
    // #190 F2 — the regression the review proved unprotected: a hosted voice whose `freq`
    // pipe carries a channel binding must stay *message-fed* (the Voicer drives freq/gate
    // by message). Hosted inertness is enforced once, in the loader's voice-resource pass
    // (bindings cleared for every copy); if that enforcement is lost and bindings ever
    // detach a hosted pipe from its message-fed materialize path again, the voice goes
    // musically dead and this note stops sounding.
    let unfed = render_bound_freq_host(4, &[]);
    assert!(
        unfed[0].iter().any(|s| s.abs() > 0.01),
        "a noted voice with a channel-bound freq pipe must sound (message-fed freq)"
    );

    // And the binding is fully inert: flooding logical channel 0 with a constant must not
    // change a single bit of the render — the host's messages, not the input master, feed
    // a hosted voice's pipes.
    let flooded = render_bound_freq_host(4, &[vec![0.33f32; BLOCK]]);
    for (ch, (a, b)) in unfed.iter().zip(flooded.iter()).enumerate() {
        for (g, (p, q)) in a.iter().zip(b.iter()).enumerate() {
            assert_eq!(
                p.to_bits(),
                q.to_bits(),
                "channel {ch} sample {g}: hosted channel bindings must be bit-inert"
            );
        }
    }
}

#[test]
fn out_of_range_channel_is_a_pointed_load_error() {
    // #190 F3: `"channel": 100000000` must fail the load with a pointed error, not size a
    // ~50 GB staging allocation in the engine. Both sides are bounded the same way.
    let none = MemoryResolver::new();
    const IN_HUGE: &str = r#"{
      "format_version": 2, "instrument": "huge",
      "interface": {
        "inputs":  { "mic": { "type": "f32_buffer", "channel": 100000000 } },
        "outputs": { "main": { "from": "/out.audio" } }
      },
      "nodes": [
        { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/mic" } } }
      ]
    }"#;
    let Err(err) = load_instrument(IN_HUGE, &Registry::builtin(), &none) else {
        panic!("a 1e8 input channel must not load");
    };
    assert!(
        matches!(&err, LoadError::InterfacePipe { name, reason }
            if name == "mic" && reason.contains("out of range")),
        "unexpected error: {err:?}"
    );

    const OUT_HUGE: &str = r#"{
      "format_version": 2, "instrument": "huge_out",
      "interface": {
        "outputs": { "main": { "from": "/osc.audio", "channel": 100000000 } }
      },
      "nodes": [ { "type": "oscillator", "address": "/osc" } ]
    }"#;
    let Err(err) = load_instrument(OUT_HUGE, &Registry::builtin(), &none) else {
        panic!("a 1e8 output channel must not load");
    };
    assert!(
        matches!(&err, LoadError::InterfacePipe { name, reason }
            if name == "main" && reason.contains("out of range")),
        "unexpected error: {err:?}"
    );

    // The boundary itself: 4095 is the last legal channel, 4096 is out.
    let ok = r#"{
      "format_version": 2, "instrument": "edge",
      "interface": {
        "inputs":  { "mic": { "type": "f32_buffer", "channel": 4095 } },
        "outputs": { "main": { "from": "/out.audio" } }
      },
      "nodes": [
        { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/mic" } } }
      ]
    }"#;
    load_instrument(ok, &Registry::builtin(), &none).expect("4095 is in range");
    let over = ok.replace("4095", "4096");
    assert!(
        load_instrument(&over, &Registry::builtin(), &none).is_err(),
        "4096 is out of range"
    );
}

#[test]
fn subpatch_inlined_channel_binding_warns_inert() {
    // #190 F4: the warning symmetry — a subpatch-inlined child's channel binding is
    // discarded at splice, exactly as inert as under a Voicer, and must say so the same
    // way (`InertChannelBinding`, nested in the hosting node).
    const PARENT: &str = r#"{
      "format_version": 2,
      "instrument": "host",
      "resources": { "fx": "gainer.json" },
      "interface": { "outputs": { "main": { "from": "/out.audio" } } },
      "nodes": [
        { "type": "subpatch", "address": "/fx", "patch": "fx" },
        { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/fx.audio" } } }
      ]
    }"#;
    let mut resolver = MemoryResolver::new();
    resolver.insert_text("gainer.json", CHILD_BOUND);
    let loaded = load_instrument(PARENT, &Registry::builtin(), &resolver).expect("load");
    let inert = loaded.warnings.iter().any(|w| {
        matches!(
            w,
            LoadWarning::Nested { node, warning }
                if node == "/fx"
                    && matches!(warning.as_ref(),
                        LoadWarning::InertChannelBinding { name } if name == "in")
        )
    });
    assert!(
        inert,
        "a subpatch-inlined channel binding warns inert, like a Voicer-hosted one: {:?}",
        loaded.warnings
    );
}

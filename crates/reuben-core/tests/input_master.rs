//! The core input master (ADR-0038 §3/§7/§10, P3 — issue #180).
//!
//! Top-level signal input pipes with a `channel` binding read the **logical input master**:
//! the caller hands `render_block_multi` one buffer per logical input channel and each bound
//! pipe copies its channel — the dual of ADR-0026's output master. These tests pin the whole
//! contract through the real load → instantiate → render path:
//!
//! - a bound pipe carries the injected channel, sample-exact, across blocks;
//! - fan-out at the master (two pipes, one channel), like output broadcast;
//! - dark-degrade (§7): no input, a missing channel, or a short buffer reads **zeros**;
//! - determinism (§10): offline render with injected input is **bit-reproducible**;
//! - inertness (§3): nested (subpatch-inlined) and Voicer-hosted channel bindings never
//!   reach the input master — the parent/host edge feeds the pipe, unfed renders silence —
//!   with the load warnings that make each dark path honest.

use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::MemoryResolver;
use reuben_core::{load_instrument, AudioConfig, LoadWarning, Registry};

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
    // ADR-0038 §3: two pipes may bind the same channel — fan-out at the master, like output
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
    // Dark-degrade (ADR-0038 §7): a caller that supplies no input at all — the `&[]` every
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
    // zeros (ADR-0038 §7) — missing reality degrades dark, never fatally and never stale.
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
    // The determinism carve-out (ADR-0038 §10): live input is a sanctioned nondeterministic
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

// A child whose pipe binds a channel — inert once nested/hosted (ADR-0038 §3).
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
    // (ADR-0038 §3 rejected it) — silence, plus the UnwiredPipe warning.
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
    // A Voicer-hosted voice's channel binding is inert (ADR-0038 §3): hosted voice plans get
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
    // ADR-0038 §7 / issue #180: an unbound-but-declared bare signal pipe at top level renders
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

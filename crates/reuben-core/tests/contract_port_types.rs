//! Compile-level proof of the port-type surface: `operator_contract!` expands the
//! `inputs { name: f32_buffer/f32 { .. }/enum(VocabType) }` grammar into a valid `Descriptor` whose
//! ports carry the right [`PortType`], single-sourced off the **shared vocab** enums (no per-op
//! enum generation any more). The macro's own unit tests assert the emitted
//! **tokens**; this test compiles them and inspects the runtime values, for the filter + oscillator
//! target contracts.
//!
//! Each invocation plants its `IN_*`/`OUT_*` consts at module scope, so each demo lives in its own
//! `mod` (real operators are already one-per-module). No `process` / no registry registration —
//! these are contract fixtures, invisible to the golden snapshot.

use reuben_core::descriptor::{Curve, PortType};
use reuben_core::vocab::{FilterMode, Waveform};

/// The filter example: a `f32_buffer` wire-in, two materialized floats (one with full meta,
/// one with unit/curve omitted), a live-switchable `enum` mode naming the shared `FilterMode`
/// vocab, and a `f32_buffer` output.
mod filter_demo {
    pub struct FilterDemo;
    reuben_core::operator_contract!(FilterDemo {
        type_name: "filter_demo",
        inputs:  { audio: f32_buffer,
                   cutoff: f32 { 20.0..=20_000.0, default 1_000.0, "Hz", exp },
                   resonance: f32 { 0.0..=1.0, default 0.2 },
                   mode: enum(FilterMode) },
        outputs: { audio: f32_buffer },
    });
}

/// The oscillator target contract: a materialized `freq` and a live `waveform` enum naming the
/// shared `Waveform` vocab.
mod osc_demo {
    pub struct OscDemo;
    reuben_core::operator_contract!(OscDemo {
        type_name: "osc_demo",
        inputs:  { freq: f32 { 20.0..=20_000.0, default 440.0, "Hz", exp },
                   waveform: enum(Waveform) },
        outputs: { audio: f32_buffer },
    });
}

#[test]
fn filter_demo_descriptor_has_the_right_port_types() {
    use filter_demo::*;
    let d = FilterDemo::contract();
    assert_eq!(d.type_name, "filter_demo");

    // Inputs number in declaration order (sequential, not per-kind), matching the handles'
    // ordinals (the consts are typed `In`/`Out` handles; `index()` is the slot).
    assert_eq!(
        (
            IN_AUDIO.index(),
            IN_CUTOFF.index(),
            IN_RESONANCE.index(),
            IN_MODE.index()
        ),
        (0, 1, 2, 3)
    );
    assert_eq!(OUT_AUDIO.index(), 0);

    // The port's Arg type follows the declaration; a `f32_buffer` is the dense per-sample wire and
    // carries no materialized default, a `f32 { .. }` is a materialized scalar control.
    assert!(matches!(d.inputs[IN_AUDIO.index()].ty, PortType::F32Buffer));
    assert!(!d.inputs[IN_AUDIO.index()].is_materialized());
    assert!(d.inputs[IN_CUTOFF.index()].is_materialized());
    assert!(matches!(
        d.inputs[IN_MODE.index()].ty,
        PortType::Vocab {
            enum_meta: Some(_),
            ..
        }
    ));
    assert!(matches!(
        d.outputs[OUT_AUDIO.index()].ty,
        PortType::F32Buffer
    ));
    assert!(d.constants.is_empty()); // the floats are inputs, and filter has no constants

    // The materialized `cutoff` carries its meta, single-sourced from the contract.
    let (i, m) = d.materialized_input("cutoff").unwrap();
    assert_eq!(
        (i, m.default, m.min, m.max),
        (IN_CUTOFF.index(), 1_000.0, 20.0, 20_000.0)
    );
    assert!(matches!(m.curve, Curve::Exponential));

    // `resonance` omitted unit/curve -> empty unit, linear default.
    let (_, r) = d.materialized_input("resonance").unwrap();
    assert_eq!(r.unit, "");
    assert!(matches!(r.curve, Curve::Linear));
}

#[test]
fn enum_input_binds_by_symbol_then_index() {
    use filter_demo::*;
    let d = FilterDemo::contract();
    let (i, e) = d.enum_input("mode").unwrap();
    assert_eq!(i, IN_MODE.index());
    assert_eq!(e.variants, FilterMode::VARIANTS); // descriptor single-sourced off the shared vocab
    assert_eq!(e.variants, ["Lp", "Hp", "Bp"]);
    assert_eq!(e.default, 0);
    assert_eq!(e.default_symbol(), "Lp");

    // Enum-over-OSC binding: symbol primary, integer index fallback.
    assert_eq!(e.resolve("Hp"), Some(1));
    assert_eq!(e.resolve("2"), Some(2)); // index fallback
    assert_eq!(e.resolve("Xx"), None); // unknown symbol
    assert_eq!(e.resolve("9"), None); // out-of-range index
}

#[test]
fn shared_vocab_enums_round_trip_and_coexist() {
    // The `mode` port references the shared `FilterMode` vocab — no per-op enum is generated.
    assert_eq!(FilterMode::DEFAULT, FilterMode::Lp);
    assert_eq!(FilterMode::default(), FilterMode::Lp);
    assert_eq!(FilterMode::from_index(2), Some(FilterMode::Bp));
    assert_eq!(FilterMode::from_index(3), None);
    assert_eq!(FilterMode::Hp.to_index(), 1);

    // The oscillator's `Waveform` is a distinct shared vocab type with its own variants.
    assert_eq!(Waveform::VARIANTS, ["Sine", "Saw"]);
    assert!(matches!(
        osc_demo::OscDemo::contract().inputs[osc_demo::IN_WAVEFORM.index()].ty,
        PortType::Vocab {
            enum_meta: Some(_),
            ..
        }
    ));
}

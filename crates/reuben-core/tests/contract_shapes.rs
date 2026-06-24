//! Compile-level proof of the ADR-0028 shape surface (Gate 1): `operator_contract!` expands the
//! new `inputs { name: float/enum { .. } }` grammar into a valid `Descriptor` *and* a usable
//! generated `Enum` type. The macro's own unit tests assert the emitted **tokens**; this test
//! compiles them and inspects the runtime values, for the filter + oscillator target contracts.
//!
//! Each invocation plants its `IN_*`/`OUT_*` consts + enum types at module scope, so each demo
//! lives in its own `mod` (real operators are already one-per-module). No `process` / no registry
//! registration — these are contract fixtures, invisible to the golden snapshot.

use reuben_core::descriptor::{Curve, Shape};

/// The ADR-0028 filter example: a bare `float` wire-in, two materialized floats (one with full
/// meta, one with unit/curve omitted), a live-switchable `enum` mode, and a `float` output.
mod filter_demo {
    pub struct FilterDemo;
    reuben_core::operator_contract!(FilterDemo {
        type_name: "filter_demo",
        inputs:  { audio: float,
                   cutoff: float { 20.0..=20_000.0, default 1_000.0, "Hz", exp },
                   resonance: float { 0.0..=1.0, default 0.2 },
                   mode: enum { Lp, Hp, Bp } },
        outputs: { audio: float },
    });
}

/// The oscillator target contract: a materialized `freq` and a live `waveform` enum.
mod osc_demo {
    pub struct OscDemo;
    reuben_core::operator_contract!(OscDemo {
        type_name: "osc_demo",
        inputs:  { freq: float { 20.0..=20_000.0, default 440.0, "Hz", exp },
                   waveform: enum { Sine, Saw } },
        outputs: { audio: float },
    });
}

#[test]
fn filter_demo_descriptor_has_the_right_shapes() {
    use filter_demo::*;
    let d = FilterDemo::contract();
    assert_eq!(d.type_name, "filter_demo");

    // Inputs number in declaration order (sequential, not per-kind), matching the consts.
    assert_eq!((IN_AUDIO, IN_CUTOFF, IN_RESONANCE, IN_MODE), (0, 1, 2, 3));
    assert_eq!(OUT_AUDIO, 0);

    // Shape follows the declaration; a bare `float` carries no materialized default.
    assert_eq!(d.inputs[IN_AUDIO].shape, Shape::Float);
    assert!(!d.inputs[IN_AUDIO].is_materialized());
    assert_eq!(d.inputs[IN_CUTOFF].shape, Shape::Float);
    assert!(d.inputs[IN_CUTOFF].is_materialized());
    assert_eq!(d.inputs[IN_MODE].shape, Shape::Enum);
    assert_eq!(d.outputs[OUT_AUDIO].shape, Shape::Float);
    assert!(d.params.is_empty()); // the floats are inputs, not params

    // The materialized `cutoff` carries its meta, single-sourced from the contract.
    let (i, m) = d.materialized_input("cutoff").unwrap();
    assert_eq!(
        (i, m.default, m.min, m.max),
        (IN_CUTOFF, 1_000.0, 20.0, 20_000.0)
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
    assert_eq!(i, IN_MODE);
    assert_eq!(e.variants, Mode::VARIANTS); // descriptor single-sourced off the generated type
    assert_eq!(e.variants, ["Lp", "Hp", "Bp"]);
    assert_eq!(e.default, 0);
    assert_eq!(e.default_symbol(), "Lp");

    // Enum-over-OSC binding (ADR-0028): symbol primary, integer index fallback.
    assert_eq!(e.resolve("Hp"), Some(1));
    assert_eq!(e.resolve("2"), Some(2)); // index fallback
    assert_eq!(e.resolve("Xx"), None); // unknown symbol
    assert_eq!(e.resolve("9"), None); // out-of-range index
}

#[test]
fn generated_enum_types_round_trip_and_coexist() {
    assert_eq!(filter_demo::Mode::DEFAULT, filter_demo::Mode::Lp);
    assert_eq!(filter_demo::Mode::default(), filter_demo::Mode::Lp);
    assert_eq!(
        filter_demo::Mode::from_index(2),
        Some(filter_demo::Mode::Bp)
    );
    assert_eq!(filter_demo::Mode::from_index(3), None);
    assert_eq!(filter_demo::Mode::Hp.to_index(), 1);

    // The oscillator's `Waveform` is a distinct generated type with its own variants.
    assert_eq!(osc_demo::Waveform::VARIANTS, ["Sine", "Saw"]);
    assert_eq!(
        osc_demo::OscDemo::contract().inputs[osc_demo::IN_WAVEFORM].shape,
        Shape::Enum
    );
}

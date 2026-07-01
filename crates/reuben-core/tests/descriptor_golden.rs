//! Golden descriptor snapshots (ADR-0025) — the oracle for the `operator_contract!` migration.
//!
//! Every built-in operator's `descriptor()` output is serialised to a canonical, human-readable
//! form and pinned in `tests/golden/descriptors.txt`. The snapshot was taken **before** any
//! operator moved to the macro; migrating an operator must leave its descriptor byte-identical, so
//! this test proves the macro reproduces — exactly — what was hand-written (port ordinals, param
//! order, curves, units). It is the test that can't lie.
//!
//! To intentionally re-bless after a deliberate descriptor change: `REUBEN_BLESS=1 cargo test -p
//! reuben-core --test descriptor_golden`.

use reuben_core::descriptor::{Curve, Descriptor, PortType};
use reuben_core::registry::Registry;

/// The carrier word for a bare port's [`PortType`] (ADR-0030/0031). Materialized `f32` and `enum`
/// **inputs** render via their own branches (`meta` / `enum_meta`); this names the carrier-style
/// ports: an `f32_buffer` audio wire ("f32_buffer"), `Note` ("message"), `Harmony` ("harmony"), and
/// an enum **output** ("enum", none today).
fn kind(ty: &PortType) -> &'static str {
    match ty {
        PortType::F32Buffer => "f32_buffer",
        PortType::F32 => "f32",
        PortType::I32 { .. } => "i32",
        PortType::Vocab { name: "Note", .. } => "message",
        PortType::Vocab {
            name: "Harmony", ..
        } => "harmony",
        PortType::Vocab {
            enum_meta: Some(_), ..
        } => "enum",
        // The type-agnostic pass-through (issue #141) — the `osc_out` sink's input.
        PortType::Arg => "arg",
        // A bare F32/I32/Str with no `meta` falls back to the f32_buffer word.
        _ => "f32_buffer",
    }
}

fn curve(c: Curve) -> &'static str {
    match c {
        Curve::Linear => "linear",
        Curve::Exponential => "exponential",
    }
}

/// One descriptor, rendered to a stable multi-line block. Floats use `{:?}` so the exact `f32`
/// value is captured (a drift in any bound would change the text).
fn render(d: &Descriptor) -> String {
    let mut s = format!("operator {}\n", d.type_name);
    for (i, p) in d.inputs.iter().enumerate() {
        // A meta-carrying input renders its own default/range. The type word comes from `kind`,
        // not a hardcoded `f32`: a materialized scalar control is `f32`, but a *signal* control
        // with a scalar default (ADR-0031 decision (a), e.g. `oscillator.freq`) is `f32_buffer`.
        // An `Enum` input renders its ordered variants + default index. Bare carrier
        // f32_buffer/message/harmony inputs (no `meta`/`enum_meta`) render via `kind`.
        match (&p.meta, p.enum_meta()) {
            (Some(m), _) => s.push_str(&format!(
                "  in[{i}] {} {} min={:?} max={:?} default={:?} unit={:?} curve={}\n",
                kind(&p.ty),
                p.name,
                m.min,
                m.max,
                m.default,
                m.unit,
                curve(m.curve)
            )),
            (_, Some(e)) => s.push_str(&format!(
                "  in[{i}] enum {} variants={:?} default={}\n",
                p.name, e.variants, e.default
            )),
            (None, None) => s.push_str(&format!("  in[{i}] {} {}\n", kind(&p.ty), p.name)),
        }
    }
    for (i, p) in d.outputs.iter().enumerate() {
        s.push_str(&format!("  out[{i}] {} {}\n", kind(&p.ty), p.name));
    }
    for (i, p) in d.constants.iter().enumerate() {
        match &p.ty {
            PortType::I32 { meta: Some(m) } => s.push_str(&format!(
                "  constant[{i}] i32 {} min={} max={} default={}\n",
                p.name, m.min, m.max, m.default
            )),
            _ => s.push_str(&format!("  constant[{i}] {} {}\n", kind(&p.ty), p.name)),
        }
    }
    for (i, r) in d.resources.iter().enumerate() {
        s.push_str(&format!("  resource[{i}] {}\n", r.name));
    }
    s
}

/// The whole built-in set, in stable (type-name) order.
fn render_all() -> String {
    Registry::builtin()
        .entries()
        .map(|e| render(&e.descriptor))
        .collect()
}

/// The formatter renders an `enum` input (variants + default) — exercised on a synthetic
/// descriptor built off a shared vocab type's [`enum_meta`](reuben_core::vocab::FilterMode::enum_meta)
/// (ADR-0030: enum metadata is single-sourced from the type, never hand-built).
#[test]
fn renders_enum_input_line() {
    use reuben_core::descriptor::{Descriptor, Port};
    use reuben_core::vocab::FilterMode;
    let d = Descriptor {
        type_name: "demo",
        inputs: vec![Port::enumerated(FilterMode::enum_meta("mode"))],
        outputs: vec![],
        constants: vec![],
        resources: vec![],
    };
    assert!(
        render(&d).contains(r#"  in[0] enum mode variants=["Lp", "Hp", "Bp"] default=0"#),
        "{}",
        render(&d)
    );
}

#[test]
fn descriptors_match_golden() {
    let golden_path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/descriptors.txt");
    let actual = render_all();

    if std::env::var_os("REUBEN_BLESS").is_some() {
        std::fs::write(golden_path, &actual).expect("write golden");
        return;
    }

    let expected = std::fs::read_to_string(golden_path).unwrap_or_else(|e| {
        panic!("missing golden {golden_path}: {e}\nfirst run: REUBEN_BLESS=1 cargo test -p reuben-core --test descriptor_golden")
    });
    assert_eq!(
        actual, expected,
        "descriptor output drifted from the golden snapshot (ADR-0025). \
         If this change is intentional, re-bless with REUBEN_BLESS=1."
    );
}

//! Golden descriptor snapshots (ADR-0025) — the oracle for the `operator_contract!` migration.
//!
//! Every built-in operator's `descriptor()` output is serialised to a canonical, human-readable
//! form and pinned in `tests/golden/descriptors.txt`. The snapshot was taken **before** any
//! operator moved to the macro; migrating an operator must leave its descriptor byte-identical, so
//! this test is what proves the macro reproduces — exactly — what was hand-written (per-kind
//! ordinals, param order, curves, units, Lane rule). It is the test that can't lie.
//!
//! To intentionally re-bless after a deliberate descriptor change: `REUBEN_BLESS=1 cargo test -p
//! reuben-core --test descriptor_golden`.

use reuben_core::descriptor::{Curve, Descriptor, LaneRule, Shape};
use reuben_core::registry::Registry;

/// The legacy carrier word for a bare port's [`Shape`] — kept stable so the golden snapshot
/// stays byte-identical now that `PortKind` is retired (ADR-0028). Materialized Float and Enum
/// inputs render via their own branches; this names the carrier-style ports (audio Float,
/// Note, Harmony).
fn kind(s: Shape) -> &'static str {
    match s {
        Shape::Float => "signal",
        Shape::Enum => "enum",
        Shape::Note => "message",
        Shape::Harmony => "context",
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
        // A new-style materialized Float input (ADR-0028) carries its own metadata; render it so
        // the snapshot captures the default/range that used to live on a same-named param. An
        // `Enum` input renders its ordered variants + default index. Legacy signal/message/context
        // inputs (no `meta`/`enum_meta`) render byte-identically to before.
        match (&p.meta, &p.enum_meta) {
            (Some(m), _) => s.push_str(&format!(
                "  in[{i}] float {} min={:?} max={:?} default={:?} unit={:?} curve={}\n",
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
            (None, None) => s.push_str(&format!("  in[{i}] {} {}\n", kind(p.shape), p.name)),
        }
    }
    for (i, p) in d.outputs.iter().enumerate() {
        s.push_str(&format!("  out[{i}] {} {}\n", kind(p.shape), p.name));
    }
    for (i, p) in d.params.iter().enumerate() {
        s.push_str(&format!(
            "  param[{i}] {} min={:?} max={:?} default={:?} unit={:?} curve={}\n",
            p.name,
            p.min,
            p.max,
            p.default,
            p.unit,
            curve(p.curve)
        ));
    }
    for (i, r) in d.resources.iter().enumerate() {
        s.push_str(&format!("  resource[{i}] {}\n", r.name));
    }
    let lanes = match d.lanes {
        LaneRule::Inherit => "inherit".to_string(),
        LaneRule::FromParam(p) => format!("from_param({p})"),
    };
    s.push_str(&format!("  lanes {lanes}\n"));
    s
}

/// The whole built-in set, in stable (type-name) order.
fn render_all() -> String {
    Registry::builtin()
        .entries()
        .map(|e| render(&e.descriptor))
        .collect()
}

/// The formatter renders an ADR-0028 `Enum` input (variants + default) — exercised here on a
/// synthetic descriptor because no built-in operator declares an `Enum` until the Phase 2 sweep.
/// This keeps the golden *machinery* ready before any real descriptor changes (and re-blesses).
#[test]
fn renders_enum_input_line() {
    use reuben_core::descriptor::{Descriptor, EnumMeta, LaneRule, Port};
    let d = Descriptor {
        type_name: "demo",
        inputs: vec![Port::enumerated(EnumMeta {
            name: "mode",
            variants: &["Lp", "Hp", "Bp"],
            default: 0,
        })],
        outputs: vec![],
        params: vec![],
        resources: vec![],
        lanes: LaneRule::Inherit,
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

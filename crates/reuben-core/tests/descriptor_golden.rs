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

use reuben_core::descriptor::{Curve, Descriptor, LaneRule, PortKind};
use reuben_core::registry::Registry;

fn kind(k: PortKind) -> &'static str {
    match k {
        PortKind::Signal => "signal",
        PortKind::Message => "message",
        PortKind::Context => "context",
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
        s.push_str(&format!("  in[{i}] {} {}\n", kind(p.kind), p.name));
    }
    for (i, p) in d.outputs.iter().enumerate() {
        s.push_str(&format!("  out[{i}] {} {}\n", kind(p.kind), p.name));
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

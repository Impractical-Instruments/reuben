//! The MVP operator set.
//!
//! Each operator lives in its own file with frozen ports/params (declared here in Stage
//! A) and is filled in test-first in Stage B. Port/param indices are part of the contract
//! the rig builder wires against — see each module's descriptor.
//!
//! The [`operator_census!`](crate::registry::operator_census) block below **is** the built-in
//! operator set: one line per module, declaring the module, re-exporting its types, and
//! registering them. Adding an operator is that one line.
//! see rules: composition-operators

// Modules that contribute no built-in operator, so they sit outside the census.
// `pipe` is an operator but deliberately unregistered: pipes are loader-built from
// `interface.inputs` and never named as document nodes.
pub mod edge;
/// Shared test helpers for the generated number operators.
#[cfg(test)]
pub mod math_test;
pub mod pipe;
/// The total arithmetic the generated number operators' scalar fns are written over.
pub mod pointwise;
/// The rounding the generated converter operators cross number types with.
pub mod rounding;

// `m::*` — a macro-generated family (`number_operator_contract!` / `unpack_op!`); the module
// carries its own `OPERATORS` array, so adding a variant stays a one-line edit *there*.
// `m::{A, B}` — hand-written operator types, named.
crate::operator_census! {
    abs::*,
    add::*,
    ceil::*,
    chord::{Chord},
    clamp::*,
    clock::{Clock},
    compressor::{Compressor},
    delay::{Delay},
    differentiate::{DifferentiateF32Signal},
    div::*,
    djfilter::{Djfilter},
    envelope::{Envelope},
    euclid::{Euclid},
    filter::{Filter},
    floor::*,
    granulator::{Granulator},
    harmony::{HarmonyOp},
    integrate::{IntegrateF32Signal},
    lfo::{Lfo},
    m2s::{M2s},
    map::*,
    max::*,
    min::*,
    modulo::*,
    mul::*,
    negate::*,
    noise::{Noise},
    osc_out::{OscOut},
    oscillator::{Oscillator},
    output::{Output},
    pan::{Pan},
    pitch2freq::{Pitch2Freq},
    power::*,
    reciprocal::*,
    resonator::{Resonator},
    reverb::{Reverb},
    round::*,
    sample::{SamplePlayer},
    saturator::{Saturator},
    sequencer::{Sequencer},
    snap::{Snap},
    strum::{Strum},
    sub::*,
    subpatch::{Subpatch},
    transpose::{Transpose},
    trunc::*,
    unpack::*,
    voicer::{Voicer},
}

/// The census's forcing function: nothing under `operators/` may define an operator the census
/// does not account for.
///
/// Folding registration into the module declaration moves it away from the definition site, so the
/// two can drift — and the drift is **silent**. An operator missing from the census still compiles,
/// still passes its own tests (those build the type directly through `OpDriver` and never consult
/// the registry), and simply never appears in `describe`, the schema, or any document. Nothing else
/// in the build notices.
///
/// Both sides are derived from source rather than restated. A hand-maintained roster of expected
/// operators would be exactly the central list this census exists to remove, and would rot the same
/// way. `bench_support`'s deliberately-unregistered `overhead` operator is out of scope by living
/// outside this directory.
#[cfg(test)]
mod census_accounts_for_every_operator {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::PathBuf;

    fn operators_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/operators")
    }

    /// Every `<stem>.rs` beside this file, `mod.rs` excluded, as `(stem, source)`.
    fn operator_sources() -> Vec<(String, String)> {
        let mut out = Vec::new();
        for entry in fs::read_dir(operators_dir()).expect("read src/operators") {
            let path = entry.expect("dir entry").path();
            if path.extension().is_some_and(|e| e == "rs") {
                let stem = path
                    .file_stem()
                    .expect("stem")
                    .to_string_lossy()
                    .into_owned();
                if stem != "mod" {
                    out.push((
                        stem,
                        fs::read_to_string(&path).expect("read operator source"),
                    ));
                }
            }
        }
        out
    }

    /// The census as *declared*, parsed from this file: module → the types it names, or `None` for
    /// the `m::*` form. Reading the source rather than `CENSUS` is deliberate — the entry form is
    /// the thing under test, and it is erased by the time the array exists.
    fn census() -> BTreeMap<String, Option<BTreeSet<String>>> {
        let src = fs::read_to_string(operators_dir().join("mod.rs")).expect("read mod.rs");
        let body = src
            .split_once("crate::operator_census! {")
            .expect("mod.rs invokes operator_census!")
            .1
            .split_once("\n}")
            .expect("the census block is closed by a brace at column 0")
            .0;

        let mut out = BTreeMap::new();
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            let entry = line
                .strip_suffix(',')
                .unwrap_or_else(|| panic!("census entry {line:?} must end in a comma"));
            let (module, rest) = entry
                .split_once("::")
                .unwrap_or_else(|| panic!("census entry {line:?} is not `module::…`"));
            let types = if rest == "*" {
                None
            } else {
                let inner = rest
                    .strip_prefix('{')
                    .and_then(|r| r.strip_suffix('}'))
                    .unwrap_or_else(|| {
                        panic!("census entry {line:?} must be `m::*` or `m::{{A}}`")
                    });
                Some(
                    inner
                        .split(',')
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .map(String::from)
                        .collect(),
                )
            };
            out.insert(module.to_string(), types);
        }
        out
    }

    /// Every hand-written `impl Operator for <T>`, as module → types.
    ///
    /// A macro-generated operator has no such line in the source — its `impl` is inside the
    /// expansion — and that asymmetry is exactly what lets these two assertions split the cases
    /// without either of them naming an operator.
    fn hand_written_operators() -> BTreeMap<String, BTreeSet<String>> {
        let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (module, src) in operator_sources() {
            for line in src.lines() {
                if let Some(rest) = line.strip_prefix("impl Operator for ") {
                    let ty: String = rest
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    out.entry(module.clone()).or_default().insert(ty);
                }
            }
        }
        out
    }

    /// Every module that mints its operators from a family macro.
    fn generated_families() -> BTreeSet<String> {
        operator_sources()
            .into_iter()
            .filter(|(_, src)| {
                src.lines().any(|l| {
                    l.starts_with("crate::number_operator_contract!(")
                        || l.starts_with("crate::unpack_op!(")
                })
            })
            .map(|(module, _)| module)
            .collect()
    }

    #[test]
    fn every_hand_written_operator_is_named_by_the_census() {
        let census = census();
        let found = hand_written_operators();
        let total: usize = found.values().map(BTreeSet::len).sum();
        assert!(
            total > 25,
            "fixture: the scan found only {total} `impl Operator` lines — the parser is broken, \
             and a broken parser makes this test pass vacuously"
        );

        let mut unaccounted = Vec::new();
        for (module, types) in &found {
            for ty in types {
                // `Pipe` is the one operator deliberately outside the census: pipes are built by
                // the loader from `interface.inputs` and can never be named as a document node.
                if (module.as_str(), ty.as_str()) == ("pipe", "Pipe") {
                    continue;
                }
                let named = matches!(census.get(module), Some(Some(n)) if n.contains(ty));
                if !named {
                    unaccounted.push(format!("{module}::{ty}"));
                }
            }
        }
        assert!(
            unaccounted.is_empty(),
            "these operators are defined but the census does not name them, so they are \
             unregistered and invisible to `describe`, the schema, and every document: \
             {unaccounted:?}"
        );

        // The exception is pinned rather than merely skipped: censusing `pipe` would put a
        // loader-built node into the document type vocabulary, and save drops nodes by that name.
        assert!(
            !census.contains_key("pipe"),
            "`pipe` must stay out of the census — it is loader-built, not patchable"
        );
    }

    #[test]
    fn a_generated_family_is_censused_by_the_star_form() {
        let census = census();
        let families = generated_families();
        assert!(
            families.len() > 10,
            "fixture: found only {} family modules — the scan is broken",
            families.len()
        );

        for module in &families {
            match census.get(module) {
                // `m::*` splices the module's own `OPERATORS`, so the macro's variant list stays
                // the single place its operators are enumerated.
                Some(None) => {}
                Some(Some(named)) => panic!(
                    "`{module}` generates its operators, so its census entry must be `{module}::*`; \
                     naming {named:?} instead silently drops every variant that list omits"
                ),
                None => panic!("`{module}` generates operators but the census does not name it"),
            }
        }

        // The converse, so the `*` form cannot spread to a module with no macro behind it — such a
        // module would need a hand-written `OPERATORS`, which the assertion above cannot see into.
        for (module, types) in &census {
            if types.is_none() {
                assert!(
                    families.contains(module),
                    "`{module}::*` splices a generated census, but `{module}` invokes no family macro"
                );
            }
        }
    }
}

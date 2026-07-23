//! The **golden projection**: run every view on a real instrument and assert the emitted bytes.
//!
//! This closes the one gap the completeness guard cannot. That guard
//! (`projection::tests::coverage`) proves every format field is *dispositioned into a view* — it
//! cannot prove the code *emits* what a coverage row claims, so coverage could lie while staying
//! green. Here the bytes are the assertion: a field that stops being rendered, an edge that stops
//! being inverted, or a marker that stops appearing shows up as a diff.
//!
//! It doubles as the surface's size record. Projection bytes **are** tokens-per-turn for an agent
//! that never reads the document (see rules: agent-mcp), so a change that quietly inflates a view
//! is a cost regression, and the golden file is where it becomes visible in review.
//!
//! The fixture is the hard case: 53 nodes, 90 interface pipes, 5 voice resources (two levels deep
//! — a voice that itself nests), and a `/clock` with five consumers, the very node whose reverse
//! edges motivated them being mandatory. It is a **frozen copy** of `instruments/acid-techno.json`
//! and its resource closure, under `tests/fixtures/projection/corpus/`, deliberately not the
//! shipped instrument: the golden asserts *the projection's* bytes, so re-voicing a shipped
//! instrument must not turn into a red build here. Refreshing the copy is a conscious act.
//!
//! Re-bless after an intended shape change with `UPDATE_PROJECTION_GOLDEN=1 cargo test -p
//! reuben-core --test projection_golden`, and **read the diff** — that is the review this test
//! exists to force.

mod common;

use common::Dir;
use reuben_core::projection::{Projector, Selection};
use reuben_core::Registry;

const GOLDEN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/projection/acid-techno.txt"
);

/// The frozen corpus root, repo-relative the way [`Dir`] wants it — the resolver root the fixture's
/// `voices/*.json` references (and their own nested references) resolve against.
const CORPUS: &str = "crates/reuben-core/tests/fixtures/projection/corpus";

fn fixture() -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/projection/corpus/acid-techno.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("the frozen fixture instrument")
}

/// Every view, in the order an agent meets them: orient on the index, read the document's intent,
/// zoom the nodes it names, then the boundary and the resource table.
fn render_all(p: &Projector) -> String {
    let sections: Vec<(String, String)> = vec![
        ("index".into(), p.index().render()),
        ("zoom /".into(), p.zoom(&Selection::names(["/"])).render()),
        // /clock: the five-consumer node. /kick_v: a voicer, so the zoom carries a nested child's
        // boundary without inlining it. /out: the node the master output pipe is fed from.
        (
            "zoom /clock /kick_v /out".into(),
            p.zoom(&Selection::names(["/clock", "/kick_v", "/out"]))
                .render(),
        ),
        (
            "zoom --type harmony".into(),
            p.zoom(&Selection::Type("harmony".into())).render(),
        ),
        ("pipes".into(), p.pipes(&Selection::All).render()),
        ("resources".into(), p.resources().render()),
    ];
    sections
        .into_iter()
        .map(|(name, body)| format!("=== {name} ===\n{body}\n"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn acid_techno_projects_to_the_golden_bytes() {
    let registry = Registry::builtin();
    let resolver = Dir(CORPUS);
    let json = fixture();
    let projector = Projector::new(&json, &registry, &resolver).expect("the fixture mints");
    let rendered = render_all(&projector);

    if std::env::var_os("UPDATE_PROJECTION_GOLDEN").is_some() {
        std::fs::create_dir_all(std::path::Path::new(GOLDEN).parent().unwrap())
            .expect("golden dir");
        std::fs::write(GOLDEN, &rendered).expect("write golden");
    }

    let golden = std::fs::read_to_string(GOLDEN).expect(
        "the golden projection (re-bless with UPDATE_PROJECTION_GOLDEN=1 if this is a new shape)",
    );
    assert_eq!(
        rendered, golden,
        "the projection's bytes changed. If that was intended, re-bless with \
         UPDATE_PROJECTION_GOLDEN=1 and review the diff — projection bytes are tokens per turn."
    );
}

/// A voice that itself nests describes its **whole** face — no re-exported port reported dark.
/// `kick-voice.json` re-exports `active` from a nested `shaped-vca` child, so the child document
/// has to be described through a resolver rebased on *it*; describing it as a fresh top-level
/// document resolves that grandchild against the root instead, and every re-exported port goes
/// falsely dark. Omitting is legal here; lying is not.
#[test]
fn a_nested_childs_boundary_is_resolved_relative_to_the_child() {
    let registry = Registry::builtin();
    let resolver = Dir(CORPUS);
    let json = fixture();
    let projector = Projector::new(&json, &registry, &resolver).expect("the fixture mints");
    let zoom = projector.zoom(&Selection::names(["/kick_v"]));
    let boundary = zoom.nodes[0].boundary.as_ref().expect("the voice's face");
    assert!(
        boundary.dark_inputs.is_empty() && boundary.dark_outputs.is_empty(),
        "kick-voice's face went dark: {:?} / {:?}",
        boundary.dark_inputs,
        boundary.dark_outputs
    );
    assert!(boundary.outputs.iter().any(|p| p.name == "active"));
}

/// The reverse edges are the load-bearing claim of the whole surface — without them the agent
/// cannot see what a destructive verb would break — so they get an assertion that does not depend
/// on the golden file's exact bytes. `/clock` feeds five sequencers in `acid-techno.json`.
#[test]
fn clock_reports_all_five_of_its_consumers() {
    let registry = Registry::builtin();
    let resolver = Dir(CORPUS);
    let json = fixture();
    let projector = Projector::new(&json, &registry, &resolver).expect("the fixture mints");
    let zoom = projector.zoom(&Selection::names(["/clock"]));
    let consumers = &zoom.nodes[0].consumers;
    assert_eq!(
        consumers.len(),
        5,
        "expected /clock's five consumers, got {consumers:?}"
    );
    assert!(consumers
        .iter()
        .all(|c| c.port.as_deref() == Some("gate") && c.input == "clock"));
}

//! The **construct** workload: synthetic documents of a chosen node count, and the
//! load + instantiate pair every Swap runs before a single block is rendered.
//!
//! The render workload in [`super`] is a *fixed* graph benched for absolute cost. This one is
//! parameterised by node count instead, because the property under test is **scaling**: the same
//! shape is built at N and 2N nodes so the growth factor between the two can be gated. A per-node
//! linear scan reads as an unremarkable per-case number in isolation and only shows up as a
//! growth factor near 4 — so a size *pair* is the measurement, not either size alone.
//!
//! The documents are generated here in the bench harness rather than frozen under `fixtures/`
//! because the perf gate never swaps `benches/` to the baseline ref: a generator that emits the
//! same JSON on both sides keeps the two runs comparable no matter which sizes a later PR picks.
//! They are emitted at `format_version` 2, the version the frozen fixtures already prove loadable
//! across the engine revisions the gate compares.
//!
//! see rules: web-product-process

use reuben_core::{AudioConfig, Plan, Registry};
use reuben_document::load_instrument;

use super::{BLOCK_SIZE, SAMPLE_RATE};

/// The one nested child [`nest_doc`] reuses: a gain cell behind an `in`/`amt` boundary, the
/// smallest thing that still mints interface pipes and so exercises splice + pipe dissolution.
/// The wide and deep shapes never reference it.
const CELL_SOURCE: &str = "cell.json";
const CELL_DOC: &str = "{\"format_version\":2,\"instrument\":\"cell\",\
     \"interface\":{\"inputs\":{\
       \"in\":{\"type\":\"f32_buffer\"},\
       \"amt\":{\"type\":\"f32\",\"default\":0.5,\"min\":0.0,\"max\":1.0}},\
     \"outputs\":{\"out\":{\"from\":\"/g.out\"}}},\
     \"nodes\":[{\"type\":\"mul_f32_signal\",\"address\":\"/g\",\
       \"inputs\":{\"a\":{\"from\":\"/in\"},\"b\":{\"from\":\"/amt\"}}}]}";

/// Serves [`CELL_DOC`] and nothing else. No sample ever resolves — the generated shapes reference
/// none — so the measurement stays parse + build + instantiate with no IO in it.
struct Generated;
impl reuben_document::resources::ResourceResolver for Generated {
    fn resolve(
        &self,
        source: &str,
    ) -> Result<reuben_core::resources::SampleBuffer, reuben_document::resources::ResolveError>
    {
        Err(reuben_document::resources::ResolveError::NotFound(
            source.to_string(),
        ))
    }
    fn resolve_text(
        &self,
        source: &str,
    ) -> Result<String, reuben_document::resources::ResolveError> {
        if source == CELL_SOURCE {
            return Ok(CELL_DOC.to_string());
        }
        Err(reuben_document::resources::ResolveError::NotFound(
            source.to_string(),
        ))
    }
}

/// Wrap generated node JSON in the document envelope, tapping `/out` as the master output.
fn document(name: &str, resources: &str, nodes: Vec<String>) -> String {
    format!(
        "{{\"format_version\":2,\"instrument\":\"{name}\",{resources}\
         \"interface\":{{\"outputs\":{{\"out\":{{\"from\":\"/out.audio\"}}}}}},\
         \"nodes\":[{}]}}",
        nodes.join(",")
    )
}

/// Reduce `level` through a binary tree of `add_f32_signal`, pushing the tree's nodes onto
/// `nodes`, and return the root's address. `leaves` is a power of two, so every level pairs
/// exactly and the tree adds `leaves - 1` nodes.
fn sum_tree(nodes: &mut Vec<String>, mut level: Vec<String>) -> String {
    let mut next = 0usize;
    while level.len() > 1 {
        let mut up = Vec::with_capacity(level.len() / 2);
        for pair in level.chunks(2) {
            let addr = format!("/s{next}");
            next += 1;
            nodes.push(format!(
                "{{\"type\":\"add_f32_signal\",\"address\":\"{addr}\",\"inputs\":\
                 {{\"a\":{{\"from\":\"{}\"}},\"b\":{{\"from\":\"{}\"}}}}}}",
                pair[0], pair[1]
            ));
            up.push(addr);
        }
        level = up;
    }
    level.pop().expect("a tree over at least one leaf")
}

/// The `output` node every shape ends on, fed from `src`.
fn output_node(src: &str) -> String {
    format!(
        "{{\"type\":\"output\",\"address\":\"/out\",\"inputs\":{{\"audio\":{{\"from\":\"{src}\"}}}}}}"
    )
}

/// **Wide**: `leaves` oscillators reduced through a binary tree of `add_f32_signal` into one
/// `output`. Total nodes `2 * leaves`; depth `log2(leaves)`. This is the shape the browser
/// harness measured — maximum edges, minimum depth — so it isolates per-edge cost from the
/// scheduler's depth handling.
pub fn wide_doc(leaves: usize) -> String {
    assert!(leaves.is_power_of_two(), "wide_doc wants a power of two");
    let mut nodes: Vec<String> = (0..leaves)
        .map(|i| {
            // Distinct frequencies so no two nodes are byte-identical; the loader has no
            // interning, but a future one must not make this workload look sublinear by accident.
            let freq = 100.0 + i as f32;
            format!(
                "{{\"type\":\"oscillator\",\"address\":\"/o{i}\",\"inputs\":{{\"freq\":{freq}}}}}"
            )
        })
        .collect();

    let level: Vec<String> = (0..leaves).map(|i| format!("/o{i}")).collect();
    let root = sum_tree(&mut nodes, level);
    nodes.push(output_node(&root));
    document("wide", "", nodes)
}

/// **Deep**: one oscillator behind a serial chain of `stages` `mul_f32_signal` nodes into one
/// `output`. Total nodes `stages + 2`; depth `stages + 2`. The dual of [`wide_doc`] — minimum
/// fan-out, maximum depth — which is where a topological sort that rescans its edge list per
/// settled node costs the most.
pub fn deep_doc(stages: usize) -> String {
    let mut nodes = vec![
        "{\"type\":\"oscillator\",\"address\":\"/o\",\"inputs\":{\"freq\":100.0}}".to_string(),
    ];
    let mut prev = "/o".to_string();
    for i in 0..stages {
        let addr = format!("/m{i}");
        nodes.push(format!(
            "{{\"type\":\"mul_f32_signal\",\"address\":\"{addr}\",\"inputs\":\
             {{\"a\":{{\"from\":\"{prev}\"}},\"b\":0.99}}}}"
        ));
        prev = addr;
    }
    nodes.push(output_node(&prev));
    document("deep", "", nodes)
}

/// **Nested**: `cells` `subpatch` nodes, all reusing the one [`CELL_DOC`] child, each fed from a
/// shared oscillator and reduced through a sum tree into one `output`. Total nodes
/// `4 * cells + 1` — every splice contributes the child's gain node plus the two boundary pipes
/// it mints, and each cell brings one tree node with it.
///
/// This is the shape the wide and deep documents cannot reach: they are large but *flat*, whereas
/// a real song is repetitive, and repetition is what puts hundreds of interface pipes in front of
/// the dissolver. Whether construct scales is a different question for a graph built by splicing
/// than for one written out longhand, so both are benched rather than one taken as proxy for the
/// other.
pub fn nest_doc(cells: usize) -> String {
    assert!(cells.is_power_of_two(), "nest_doc wants a power of two");
    let mut nodes = vec![
        "{\"type\":\"oscillator\",\"address\":\"/o\",\"inputs\":{\"freq\":100.0}}".to_string(),
    ];
    for i in 0..cells {
        // Distinct `amt` per cell, so no two splices are byte-identical.
        let amt = 0.1 + 0.8 * (i % 8) as f32 / 8.0;
        nodes.push(format!(
            "{{\"type\":\"subpatch\",\"address\":\"/c{i}\",\"patch\":\"cell\",\"inputs\":\
             {{\"in\":{{\"from\":\"/o\"}},\"amt\":{amt}}}}}"
        ));
    }
    let level: Vec<String> = (0..cells).map(|i| format!("/c{i}.out")).collect();
    let root = sum_tree(&mut nodes, level);
    nodes.push(output_node(&root));
    document(
        "nest",
        &format!("\"resources\":{{\"cell\":\"{CELL_SOURCE}\"}},"),
        nodes,
    )
}

/// The measured region: parse + build + instantiate, exactly the engine work
/// `Coordinator::install_initial` does around its manifest bookkeeping. `expect_nodes` is the
/// graph the caller's shape is supposed to have produced. Returns the Plan's arena buffer count —
/// a size-proportional witness of the built Plan, for the caller to `black_box`.
///
/// Both assertions are load-bearing, because the *shape* of this workload is not something either
/// gate can check. A resource that fails to resolve is deliberately non-fatal — a `patch` the
/// resolver stops serving degrades to a warning and a graph with the nested nodes missing — so if
/// [`Generated`] ever stopped matching, `nest` would quietly collapse from 1024 interface pipes to
/// none. It would still build, still scale linearly (a degenerate graph is linear), and still read
/// as a large *improvement* against the baseline: green on both gates, while the only case that
/// exercises pipe dissolution had stopped exercising it. The same argument `perf-gate.sh` makes for
/// swapping `instruments/` with `src/` — a workload that mis-loads into something cheap is worse
/// than one that fails — applies here, and `benches/` is never swapped, so it is made explicit
/// instead.
pub fn construct(json: &str, expect_nodes: usize) -> usize {
    let loaded = load_instrument(json, &Registry::builtin(), &Generated).expect("doc loads");
    assert!(
        loaded.warnings.is_empty(),
        "the workload degraded instead of loading: {:?}",
        loaded.warnings
    );
    assert_eq!(
        loaded.graph.nodes.len(),
        expect_nodes,
        "the workload built a different graph than its size says"
    );
    let plan = Plan::instantiate(loaded.graph, AudioConfig::new(SAMPLE_RATE, BLOCK_SIZE))
        .expect("doc instantiates");
    plan.num_buffers
}

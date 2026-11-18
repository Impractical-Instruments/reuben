//! Where every box goes. A box whose position the document stores keeps it; the rest are placed
//! here, by one rule, from nothing but the graph — so the same document always opens the same.
//!
//! The rule, for the boxes with no stored position:
//!
//! 1. **Columns by signal flow, left to right.** A box sits one column right of the rightmost
//!    box feeding it. A box nothing feeds — an interface input, a clock, an LFO — sits one column
//!    left of the leftmost box it feeds, rather than all of them piling into the first column.
//!    A wire that closes a feedback loop is ignored for this step.
//! 2. **Rows by neighbours.** Within a column, boxes are ordered by the mean height of the boxes
//!    they are wired to, swept left-to-right then right-to-left [`SWEEPS`] times, starting from
//!    document order. Ties keep their order.
//! 3. **Packing.** Each column is as wide as its widest box and is centred on one horizontal
//!    line; columns are [`COLUMN_GAP`] apart and boxes in a column [`ROW_GAP`] apart.
//! 4. **An interface input wired to exactly one port sits beside that port**, [`BESIDE_GAP`] to
//!    its left on the same line, and takes no part in steps 1–3. A sequencer's sixteen step
//!    inputs line up down the side of the node they set instead of filling a column of their own.
//!
//! When some boxes have stored positions and some do not, the placed block goes directly below
//! everything stored, so it never lands on a box the user arranged.

use std::collections::{BTreeMap, BTreeSet};

use eframe::egui::{Pos2, Rect};

use super::geometry::{bounds, input_pos, size};
use super::graph::{Graph, Item, PortRef};

/// Wide enough that a wire between adjacent columns has room to curve between port rows.
pub const COLUMN_GAP: f32 = 72.0;

pub const ROW_GAP: f32 = 16.0;

/// Room for a short level wire between an interface input and the port it sits beside.
pub const BESIDE_GAP: f32 = 24.0;

/// Rounds of row ordering. The order settles within a few on the library's instruments; more
/// buys nothing visible.
const SWEEPS: usize = 4;

/// The rectangle of every box of `graph`, in canvas units, indexed as `graph.boxes`.
pub fn place(graph: &Graph) -> Vec<Rect> {
    let beside = beside(graph);
    let flow = Flow::of(graph, &beside);
    let columns = flow.columns();
    let mut rects: Vec<Rect> = graph
        .boxes
        .iter()
        .map(|b| Rect::from_min_size(b.stored.unwrap_or(Pos2::ZERO), size(b)))
        .collect();

    let mut placed: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, b) in graph.boxes.iter().enumerate() {
        if b.stored.is_none() && beside[i].is_none() {
            placed.entry(columns[i]).or_default().push(i);
        }
    }
    let mut placed: Vec<Vec<usize>> = placed.into_values().collect();

    // Per box, the room to its left that the interface inputs beside it take.
    let mut lead = vec![0.0_f32; rects.len()];
    for (i, port) in beside.iter().enumerate() {
        if let Some(port) = port {
            lead[port.node] = lead[port.node].max(rects[i].width() + BESIDE_GAP);
        }
    }

    let mut x = 0.0;
    for column in &placed {
        let lead = column.iter().map(|&i| lead[i]).fold(0.0, f32::max);
        for &i in column {
            rects[i] = Rect::from_min_size(Pos2::new(x + lead, 0.0), rects[i].size());
        }
        let width = column.iter().map(|&i| rects[i].width()).fold(0.0, f32::max);
        x += lead + width + COLUMN_GAP;
        stack(column, &mut rects);
    }
    for _ in 0..SWEEPS {
        for column in placed.iter_mut() {
            order(column, &flow.feeders, &mut rects);
        }
        for column in placed.iter_mut().rev() {
            order(column, &flow.fed, &mut rects);
        }
    }
    for (i, port) in beside.iter().enumerate() {
        if let Some(port) = port {
            let at = input_pos(&graph.boxes[port.node], rects[port.node], port.port);
            let size = rects[i].size();
            let corner = Pos2::new(at.x - BESIDE_GAP - size.x, at.y - size.y / 2.0);
            rects[i] = Rect::from_min_size(corner, size);
        }
    }

    let stored = |yes: bool| {
        let boxes = graph.boxes.iter().zip(&rects);
        bounds(boxes.filter_map(|(b, r)| (b.stored.is_some() == yes).then_some(*r)))
    };
    if let Some(block) = stored(false) {
        let corner = match stored(true) {
            Some(stored) => Pos2::new(stored.left(), stored.bottom() + COLUMN_GAP),
            None => Pos2::ZERO,
        };
        let shift = corner - block.min;
        for (b, rect) in graph.boxes.iter().zip(&mut rects) {
            if b.stored.is_none() {
                *rect = rect.translate(shift);
            }
        }
    }
    rects
}

/// Per box, the input port it sits beside: set for an interface input the document stores no
/// position for, wired to exactly one port, of a box that is itself placed here.
fn beside(graph: &Graph) -> Vec<Option<PortRef>> {
    let mut targets: Vec<Vec<PortRef>> = vec![Vec::new(); graph.boxes.len()];
    for wire in &graph.wires {
        targets[wire.from.node].push(wire.to);
    }
    (graph.boxes.iter().zip(&targets))
        .map(
            |(b, targets)| match (&b.item, b.stored, targets.as_slice()) {
                (Item::Input(_), None, [only]) if graph.boxes[only.node].stored.is_none() => {
                    Some(*only)
                }
                _ => None,
            },
        )
        .collect()
}

/// Lay `column` out top to bottom in its current order, centred on `y = 0`.
fn stack(column: &[usize], rects: &mut [Rect]) {
    let height: f32 = column.iter().map(|&i| rects[i].height()).sum::<f32>()
        + ROW_GAP * column.len().saturating_sub(1) as f32;
    let mut y = -height / 2.0;
    for &i in column {
        let r = rects[i];
        rects[i] = Rect::from_min_size(Pos2::new(r.left(), y), r.size());
        y += r.height() + ROW_GAP;
    }
}

/// Reorder `column` by the mean height of each box's `neighbours`, and restack it. A box with no
/// neighbours on that side holds its height.
fn order(column: &mut [usize], neighbours: &[Vec<usize>], rects: &mut [Rect]) {
    let key = |i: usize| {
        let near = &neighbours[i];
        if near.is_empty() {
            rects[i].center().y
        } else {
            near.iter().map(|&n| rects[n].center().y).sum::<f32>() / near.len() as f32
        }
    };
    let mut keyed: Vec<(f32, usize)> = column.iter().map(|&i| (key(i), i)).collect();
    keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (slot, (_, i)) in column.iter_mut().zip(keyed) {
        *slot = i;
    }
    stack(column, rects);
}

/// Which box feeds which, with the wires that close a feedback loop left out, so that following
/// `fed` from any box ends. A box that sits beside a port is left out too: it follows its port
/// rather than the flow.
struct Flow {
    /// Per box, the boxes wired into it.
    feeders: Vec<Vec<usize>>,
    /// Per box, the boxes it is wired into.
    fed: Vec<Vec<usize>>,
    /// Every box, each after all of its feeders.
    downstream: Vec<usize>,
}

impl Flow {
    fn of(graph: &Graph, beside: &[Option<PortRef>]) -> Self {
        let n = graph.boxes.len();
        let wired: BTreeSet<(usize, usize)> = graph
            .wires
            .iter()
            .map(|w| (w.from.node, w.to.node))
            .filter(|&(from, to)| from != to && beside[from].is_none())
            .collect();
        let mut all: Vec<Vec<usize>> = vec![Vec::new(); n];
        for &(from, to) in &wired {
            all[from].push(to);
        }

        // A depth-first walk in box order. A wire into a box the walk is still inside closes a
        // loop and is dropped; which wire of a loop that is depends only on document order.
        #[derive(Clone, Copy, PartialEq)]
        enum Seen {
            No,
            Inside,
            Done,
        }
        let mut seen = vec![Seen::No; n];
        let mut feeders = vec![Vec::new(); n];
        let mut fed = vec![Vec::new(); n];
        let mut finished = Vec::with_capacity(n);
        for root in 0..n {
            if seen[root] != Seen::No {
                continue;
            }
            seen[root] = Seen::Inside;
            let mut walk = vec![(root, 0)];
            while let Some((at, next)) = walk.last_mut() {
                let at = *at;
                let Some(&to) = all[at].get(*next) else {
                    seen[at] = Seen::Done;
                    finished.push(at);
                    walk.pop();
                    continue;
                };
                *next += 1;
                if seen[to] == Seen::Inside {
                    continue;
                }
                fed[at].push(to);
                feeders[to].push(at);
                if seen[to] == Seen::No {
                    seen[to] = Seen::Inside;
                    walk.push((to, 0));
                }
            }
        }
        finished.reverse();
        for list in &mut feeders {
            list.sort_unstable();
        }
        Self {
            feeders,
            fed,
            downstream: finished,
        }
    }

    /// The column of every box, counted from 0 at the left.
    fn columns(&self) -> Vec<usize> {
        let mut columns = vec![0; self.feeders.len()];
        for &i in &self.downstream {
            columns[i] = (self.feeders[i].iter())
                .map(|&f| columns[f] + 1)
                .max()
                .unwrap_or(0);
        }
        // Nothing these feed is itself unfed, so moving one never moves what another is measured
        // against.
        for i in 0..columns.len() {
            if self.feeders[i].is_empty() {
                if let Some(nearest) = self.fed[i].iter().map(|&f| columns[f]).min() {
                    columns[i] = nearest - 1;
                }
            }
        }
        columns
    }
}

/// Whether two rectangles share any area. Touching edges do not count.
#[cfg(test)]
pub fn overlap(a: Rect, b: Rect) -> bool {
    a.left() < b.right() && b.left() < a.right() && a.top() < b.bottom() && b.top() < a.bottom()
}

/// Every pair of `rects` that overlaps, by index.
#[cfg(test)]
pub fn overlaps(rects: &[Rect]) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    for a in 0..rects.len() {
        for b in a + 1..rects.len() {
            if overlap(rects[a], rects[b]) {
                found.push((a, b));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::graph::tests::{small, structure};
    use super::super::graph::Item;
    use super::*;
    use reuben_api::authoring::{InputValue, InstrumentStructure, NodeInput, Position, WireSource};

    fn rect_of(graph: &Graph, rects: &[Rect], item: Item) -> Rect {
        rects[graph.index_of(&item).expect("the box exists")]
    }

    /// Nodes named by `wires`' ends, each with one `in` and one `out`, wired as `wires` says.
    fn chain(nodes: &[&str], wires: &[(&str, &str)]) -> InstrumentStructure {
        let nodes: Vec<serde_json::Value> = nodes
            .iter()
            .map(|address| {
                let inputs: Vec<serde_json::Value> = wires
                    .iter()
                    .filter(|(_, to)| to == address)
                    .enumerate()
                    .map(|(i, (from, _))| {
                        json!({ "name": format!("in{i}"), "value": { "kind": "wire", "from":
                            { "kind": "node_output", "node": from, "port": "out" } } })
                    })
                    .collect();
                json!({
                    "address": address,
                    "type_name": "thing",
                    "inputs": inputs,
                    "ports": {
                        "inputs": [],
                        "outputs": [{ "name": "out", "kind": "signal" }],
                    },
                })
            })
            .collect();
        structure(json!({
            "instrument": "chain",
            "hash": "h",
            "nodes": nodes,
            "inputs": [],
            "outputs": [],
            "resources": [],
            "report": { "ok": true, "errors": [], "warnings": [] },
        }))
    }

    #[test]
    fn signal_flows_left_to_right() {
        let graph = Graph::from_structure(&small());
        let rects = place(&graph);
        let at = |item| rect_of(&graph, &rects, item);
        let osc = at(Item::Node("/osc".into()));
        let filter = at(Item::Node("/filter".into()));
        let main = at(Item::Output("main".into()));
        let cutoff = at(Item::Input("cutoff".into()));
        assert!(osc.right() < filter.left());
        assert!(cutoff.right() < filter.left());
        assert!(filter.right() < main.left());
        assert!(overlaps(&rects).is_empty());
    }

    #[test]
    fn a_box_is_the_size_its_contents_need() {
        let graph = Graph::from_structure(&small());
        let rects = place(&graph);
        for (b, r) in graph.boxes.iter().zip(&rects) {
            // A rect holds two corners, so its size comes back through a subtraction.
            assert!((r.size() - size(b)).length() < 1e-3, "{:?}", b.item);
        }
    }

    #[test]
    fn the_same_graph_is_placed_the_same_way_every_time() {
        let graph = Graph::from_structure(&small());
        assert_eq!(place(&graph), place(&graph));
        assert_eq!(place(&Graph::from_structure(&small())), place(&graph));
    }

    #[test]
    fn a_placed_block_starts_at_the_origin() {
        let graph = Graph::from_structure(&small());
        let all = bounds(place(&graph)).unwrap();
        assert_eq!(all.min, Pos2::ZERO);
    }

    #[test]
    fn an_unfed_box_sits_just_left_of_what_it_feeds() {
        // `/lfo` feeds only `/d`, the end of a chain three long.
        let doc = chain(
            &["/a", "/b", "/c", "/d", "/lfo"],
            &[("/a", "/b"), ("/b", "/c"), ("/c", "/d"), ("/lfo", "/d")],
        );
        let graph = Graph::from_structure(&doc);
        let rects = place(&graph);
        let at = |address: &str| rect_of(&graph, &rects, Item::Node(address.into()));
        assert_eq!(at("/lfo").left(), at("/c").left());
        assert!(at("/a").left() < at("/b").left());
        assert!(overlaps(&rects).is_empty());
    }

    #[test]
    fn an_interface_input_wired_to_one_port_sits_beside_it() {
        let graph = Graph::from_structure(&small());
        let rects = place(&graph);
        let filter = graph.index_of(&Item::Node("/filter".into())).unwrap();
        let cutoff = rect_of(&graph, &rects, Item::Input("cutoff".into()));
        let port = input_pos(&graph.boxes[filter], rects[filter], 1);
        assert_eq!(graph.boxes[filter].inputs[1].name, "cutoff");
        assert_eq!(cutoff.right(), port.x - BESIDE_GAP);
        assert_eq!(cutoff.center().y, port.y);
    }

    #[test]
    fn interface_inputs_beside_adjacent_ports_do_not_touch() {
        let mut doc = small();
        let mut q = doc.inputs[0].clone();
        q.port.name = "q".into();
        doc.inputs.push(q);
        doc.nodes[1].inputs[2].value = InputValue::Wire {
            from: WireSource::InterfaceInput { name: "q".into() },
        };
        let graph = Graph::from_structure(&doc);
        let rects = place(&graph);
        let cutoff = rect_of(&graph, &rects, Item::Input("cutoff".into()));
        let q = rect_of(&graph, &rects, Item::Input("q".into()));
        assert_eq!(cutoff.right(), q.right());
        assert!(cutoff.bottom() < q.top());
        assert!(overlaps(&rects).is_empty());
    }

    #[test]
    fn an_interface_input_wired_to_two_ports_takes_a_column_left_of_both() {
        let mut doc = small();
        doc.nodes[0].inputs.push(NodeInput {
            name: "freq".into(),
            value: InputValue::Wire {
                from: WireSource::InterfaceInput {
                    name: "cutoff".into(),
                },
            },
        });
        let graph = Graph::from_structure(&doc);
        let rects = place(&graph);
        let at = |item| rect_of(&graph, &rects, item);
        let cutoff = at(Item::Input("cutoff".into()));
        let osc = at(Item::Node("/osc".into()));
        assert_eq!(cutoff.left(), 0.0);
        assert!(cutoff.right() + COLUMN_GAP <= osc.left());
        assert!(osc.right() < at(Item::Node("/filter".into())).left());
        assert!(overlaps(&rects).is_empty());
    }

    #[test]
    fn a_box_wired_to_nothing_is_still_placed_clear_of_the_rest() {
        let doc = chain(&["/a", "/b", "/alone"], &[("/a", "/b")]);
        let graph = Graph::from_structure(&doc);
        let rects = place(&graph);
        assert!(overlaps(&rects).is_empty());
        assert!(rects.iter().all(|r| r.is_finite()));
    }

    #[test]
    fn a_feedback_loop_is_placed_without_overlap() {
        let doc = chain(
            &["/a", "/b", "/c"],
            &[("/a", "/b"), ("/b", "/c"), ("/c", "/a"), ("/b", "/b")],
        );
        let graph = Graph::from_structure(&doc);
        let rects = place(&graph);
        assert!(overlaps(&rects).is_empty());
        let at = |address: &str| rect_of(&graph, &rects, Item::Node(address.into()));
        // The loop is cut at the wire back into the first node in document order.
        assert!(at("/a").right() < at("/b").left());
        assert!(at("/b").right() < at("/c").left());
    }

    #[test]
    fn rows_follow_the_boxes_they_are_wired_to() {
        // Two chains declared interleaved and crossed: `/a1 → /b1`, `/a2 → /b2`, with the second
        // column in the opposite document order from the first.
        let doc = chain(
            &["/a1", "/a2", "/b2", "/b1"],
            &[("/a1", "/b1"), ("/a2", "/b2")],
        );
        let graph = Graph::from_structure(&doc);
        let rects = place(&graph);
        let at = |address: &str| rect_of(&graph, &rects, Item::Node(address.into()));
        assert_eq!(
            at("/a1").top() < at("/a2").top(),
            at("/b1").top() < at("/b2").top()
        );
    }

    #[test]
    fn a_stored_position_is_kept_exactly() {
        let mut doc = small();
        for (i, node) in doc.nodes.iter_mut().enumerate() {
            node.layout = Some(Position {
                x: 500.0 * i as f32 - 250.0,
                y: 33.0,
            });
        }
        doc.inputs[0].layout = Some(Position { x: -900.0, y: 0.0 });
        doc.outputs[0].layout = Some(Position { x: 900.0, y: 0.0 });
        let graph = Graph::from_structure(&doc);
        let rects = place(&graph);
        for (b, r) in graph.boxes.iter().zip(&rects) {
            assert_eq!(Some(r.min), b.stored);
        }
    }

    #[test]
    fn boxes_without_a_stored_position_are_placed_below_those_with_one() {
        let mut doc = small();
        // Stored right where the placement would otherwise start.
        doc.nodes[1].layout = Some(Position { x: 0.0, y: 0.0 });
        let graph = Graph::from_structure(&doc);
        let rects = place(&graph);
        let filter = rect_of(&graph, &rects, Item::Node("/filter".into()));
        assert_eq!(filter.min, Pos2::ZERO);
        assert!(overlaps(&rects).is_empty());
        for (b, r) in graph.boxes.iter().zip(&rects) {
            if b.stored.is_none() {
                assert!(r.top() > filter.bottom(), "{:?}", b.item);
            }
        }
    }

    #[test]
    fn an_empty_graph_places_nothing() {
        assert!(place(&Graph::default()).is_empty());
    }
}

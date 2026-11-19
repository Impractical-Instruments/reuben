//! Where things are on the canvas, in canvas units: how big a box is, where each port sits on it,
//! the curve a wire follows, and which box a point is over. Nothing here draws.

use eframe::egui::{Pos2, Rect, Vec2};

use super::graph::{Graph, NodeBox, Port, PortRef, Wire};

/// The size, in canvas units, of the monospace text every label is drawn in.
pub const TEXT_SIZE: f32 = 12.0;

/// The advance of one glyph of egui's monospace font (Hack: 0.602 em) at [`TEXT_SIZE`], rounded
/// up so an estimated label is never narrower than the drawn one.
const GLYPH_WIDTH: f32 = 7.3;

/// Height of one line of the header, and of one port row.
pub const LINE: f32 = 18.0;

/// Height of an interface pipe's box. Less than [`LINE`], so that pipes sitting beside adjacent
/// port rows of one node do not touch.
const PIPE_HEIGHT: f32 = LINE - 2.0;

const MIN_PIPE_WIDTH: f32 = 40.0;

/// Space between a box's edge and its text, on every side.
pub const PAD: f32 = 8.0;

/// Space between the header and the first port row.
const HEADER_GAP: f32 = 4.0;

/// The least space between an input's label and the output's label on the same row.
const LABEL_GAP: f32 = 16.0;

const MIN_WIDTH: f32 = 96.0;

pub const PORT_RADIUS: f32 = 4.0;

/// The shortest a wire's control handles get, so a wire between two close ports still leaves and
/// arrives horizontally.
const MIN_HANDLE: f32 = 24.0;

/// The longest a wire's control handles get, so a wire running backwards or across the whole
/// canvas does not balloon above and below its ends.
const MAX_HANDLE: f32 = 160.0;

fn text_width(text: &str) -> f32 {
    text.chars().count() as f32 * GLYPH_WIDTH
}

/// A node's address line and type line, with the space above them.
const HEADER_HEIGHT: f32 = PAD + 2.0 * LINE;

/// An interface pipe is one line holding its name, its one port at mid-height. A node is a header
/// over one row per port on its longer side.
pub fn size(b: &NodeBox) -> Vec2 {
    if b.is_boundary() {
        let width = (text_width(&b.title) + 2.0 * PAD).max(MIN_PIPE_WIDTH);
        return Vec2::new(width, PIPE_HEIGHT);
    }
    let label = |i: usize, ports: &[Port]| ports.get(i).map_or(0.0, |p| text_width(&p.name));
    let rows = b.inputs.len().max(b.outputs.len());
    let widest_row = (0..rows)
        .map(|i| label(i, &b.inputs) + LABEL_GAP + label(i, &b.outputs))
        .fold(0.0, f32::max);
    let text = text_width(&b.title)
        .max(text_width(&b.subtitle))
        .max(widest_row);
    let ports = match rows {
        0 => 0.0,
        n => HEADER_GAP + n as f32 * LINE,
    };
    Vec2::new(
        (text + 2.0 * PAD).max(MIN_WIDTH),
        HEADER_HEIGHT + ports + PAD,
    )
}

/// The centre line of port row `port` of a box whose top is at `top`.
fn port_y(b: &NodeBox, top: f32, port: usize) -> f32 {
    if b.is_boundary() {
        top + PIPE_HEIGHT / 2.0
    } else {
        top + HEADER_HEIGHT + HEADER_GAP + (port as f32 + 0.5) * LINE
    }
}

/// Where input `port` of `b` is, on the left edge of its `rect`.
pub fn input_pos(b: &NodeBox, rect: Rect, port: usize) -> Pos2 {
    Pos2::new(rect.left(), port_y(b, rect.top(), port))
}

/// Where output `port` of `b` is, on the right edge of its `rect`.
pub fn output_pos(b: &NodeBox, rect: Rect, port: usize) -> Pos2 {
    Pos2::new(rect.right(), port_y(b, rect.top(), port))
}

/// The left ends of the centre lines of a node's two header lines: its address, then its type.
pub fn header_text(rect: Rect) -> [Pos2; 2] {
    let x = rect.left() + PAD;
    let first = rect.top() + PAD / 2.0 + LINE / 2.0;
    [Pos2::new(x, first), Pos2::new(x, first + LINE)]
}

/// The control points of the cubic Bézier `wire` follows between its two ports.
pub fn wire_curve(graph: &Graph, rects: &[Rect], wire: &Wire) -> [Pos2; 4] {
    let PortRef { node, port } = wire.from;
    let from = output_pos(&graph.boxes[node], rects[node], port);
    let PortRef { node, port } = wire.to;
    let to = input_pos(&graph.boxes[node], rects[node], port);
    curve(from, to)
}

/// The control points of the cubic Bézier a wire follows from an output at `from` to an input at
/// `to`.
pub fn curve(from: Pos2, to: Pos2) -> [Pos2; 4] {
    let handle = ((to.x - from.x).abs() / 2.0).clamp(MIN_HANDLE, MAX_HANDLE);
    [
        from,
        from + Vec2::new(handle, 0.0),
        to - Vec2::new(handle, 0.0),
        to,
    ]
}

/// The output port whose dot is nearest `point`, if one is within `reach`.
pub fn output_at(graph: &Graph, rects: &[Rect], point: Pos2, reach: f32) -> Option<PortRef> {
    nearest_port(graph, rects, point, reach, |b| b.outputs.len(), output_pos)
}

/// The input port whose dot is nearest `point`, if one is within `reach`.
pub fn input_at(graph: &Graph, rects: &[Rect], point: Pos2, reach: f32) -> Option<PortRef> {
    nearest_port(graph, rects, point, reach, |b| b.inputs.len(), input_pos)
}

fn nearest_port(
    graph: &Graph,
    rects: &[Rect],
    point: Pos2,
    reach: f32,
    count: impl Fn(&NodeBox) -> usize,
    at: impl Fn(&NodeBox, Rect, usize) -> Pos2,
) -> Option<PortRef> {
    let mut best: Option<(f32, PortRef)> = None;
    for (node, (b, rect)) in graph.boxes.iter().zip(rects).enumerate() {
        for port in 0..count(b) {
            let distance = at(b, *rect, port).distance(point);
            if distance <= reach && best.is_none_or(|(d, _)| distance < d) {
                best = Some((distance, PortRef { node, port }));
            }
        }
    }
    best.map(|(_, port)| port)
}

/// Straight pieces a wire's curve is cut into to measure how near a point is to it: fine enough
/// that the cut never strays a pixel from the drawn curve at the zooms text is shown at.
const WIRE_PIECES: usize = 32;

/// The wire passing nearest `point`, if one passes within `reach`.
pub fn wire_at(graph: &Graph, rects: &[Rect], point: Pos2, reach: f32) -> Option<usize> {
    let mut best: Option<(f32, usize)> = None;
    for (i, wire) in graph.wires.iter().enumerate() {
        let [a, b, c, d] = wire_curve(graph, rects, wire);
        let along = |t: f32| {
            let u = 1.0 - t;
            (a.to_vec2() * (u * u * u)
                + b.to_vec2() * (3.0 * u * u * t)
                + c.to_vec2() * (3.0 * u * t * t)
                + d.to_vec2() * (t * t * t))
                .to_pos2()
        };
        let mut from = a;
        for step in 1..=WIRE_PIECES {
            let to = along(step as f32 / WIRE_PIECES as f32);
            let distance = segment_distance(point, from, to);
            if distance <= reach && best.is_none_or(|(d, _)| distance < d) {
                best = Some((distance, i));
            }
            from = to;
        }
    }
    best.map(|(_, i)| i)
}

fn segment_distance(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = if ab.length_sq() == 0.0 {
        0.0
    } else {
        ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0)
    };
    p.distance(a + ab * t)
}

/// The box under `point`. Boxes are painted in order, so where two overlap the later one is on
/// top and is the one hit.
pub fn hit(rects: &[Rect], point: Pos2) -> Option<usize> {
    rects.iter().rposition(|r| r.contains(point))
}

/// The smallest rectangle holding every one of `rects`; `None` when there are none.
pub fn bounds(rects: impl IntoIterator<Item = Rect>) -> Option<Rect> {
    rects.into_iter().reduce(|all, r| all.union(r))
}

#[cfg(test)]
mod tests {
    use super::super::graph::tests::small;
    use super::super::graph::{Item, PortKind};
    use super::*;

    fn graph() -> Graph {
        Graph::from_structure(&small())
    }

    fn rect_of(b: &NodeBox, at: Pos2) -> Rect {
        Rect::from_min_size(at, size(b))
    }

    #[test]
    fn a_box_is_wide_enough_for_its_longest_label() {
        let mut b = graph().boxes[2].clone();
        let before = size(&b);
        b.title = "/a_very_long_address_that_no_default_width_holds".into();
        let after = size(&b);
        assert!(after.x > before.x);
        assert!(after.x >= text_width(&b.title) + 2.0 * PAD);
        assert_eq!(after.y, before.y);
    }

    #[test]
    fn a_box_grows_a_row_for_every_port_on_its_longer_side() {
        let g = graph();
        let (osc, filter) = (&g.boxes[1], &g.boxes[2]);
        assert_eq!(size(filter).y - size(osc).y, 2.0 * LINE);
    }

    #[test]
    fn inputs_sit_on_the_left_edge_and_outputs_on_the_right_inside_the_box() {
        let g = graph();
        for b in &g.boxes {
            let rect = rect_of(b, Pos2::new(100.0, 50.0));
            for i in 0..b.inputs.len() {
                let p = input_pos(b, rect, i);
                assert_eq!(p.x, rect.left());
                assert!(rect.top() < p.y && p.y < rect.bottom(), "{:?}", b.item);
            }
            for i in 0..b.outputs.len() {
                let p = output_pos(b, rect, i);
                assert_eq!(p.x, rect.right());
                assert!(rect.top() < p.y && p.y < rect.bottom(), "{:?}", b.item);
            }
        }
    }

    #[test]
    fn a_box_with_more_outputs_than_inputs_holds_every_output() {
        let mut b = graph().boxes[1].clone();
        let one = size(&b);
        let extra = Port {
            name: "a_second_output_with_a_long_name".into(),
            kind: PortKind::Signal,
        };
        b.outputs.extend([extra.clone(), extra]);
        assert!(b.outputs.len() > b.inputs.len());
        let grown = size(&b);
        assert_eq!(grown.y - one.y, 2.0 * LINE);
        assert!(grown.x >= LABEL_GAP + text_width(&b.outputs[1].name) + 2.0 * PAD);
        let rect = rect_of(&b, Pos2::ZERO);
        for i in 0..b.outputs.len() {
            let y = output_pos(&b, rect, i).y;
            assert!(rect.top() < y && y < rect.bottom(), "output {i}");
        }
    }

    #[test]
    fn port_rows_are_one_line_apart_below_the_header() {
        let g = graph();
        let filter = &g.boxes[2];
        let rect = rect_of(filter, Pos2::ZERO);
        let ys = [0, 1, 2].map(|i| input_pos(filter, rect, i).y);
        assert!(ys[0] >= header_text(rect)[1].y + LINE);
        assert_eq!(ys[1] - ys[0], LINE);
        assert_eq!(ys[2] - ys[1], LINE);
        // An input and the output on its row share a line.
        assert_eq!(output_pos(filter, rect, 0).y, ys[0]);
    }

    #[test]
    fn an_interface_pipe_is_shorter_than_a_port_row_with_its_port_at_mid_height() {
        let g = graph();
        let cutoff = &g.boxes[0];
        assert_eq!(cutoff.item, Item::Input("cutoff".into()));
        let rect = rect_of(cutoff, Pos2::ZERO);
        assert!(rect.height() < LINE);
        assert_eq!(output_pos(cutoff, rect, 0).y, rect.center().y);
    }

    #[test]
    fn a_wire_runs_from_its_output_port_to_its_input_port_leaving_and_arriving_level() {
        let g = graph();
        let rects: Vec<Rect> = g
            .boxes
            .iter()
            .enumerate()
            .map(|(i, b)| rect_of(b, Pos2::new(i as f32 * 300.0, i as f32 * 40.0)))
            .collect();
        let wire = &g.wires[0];
        assert_eq!(wire.kind, PortKind::Signal);
        let [a, b, c, d] = wire_curve(&g, &rects, wire);
        assert_eq!(a, output_pos(&g.boxes[1], rects[1], 0));
        assert_eq!(d, input_pos(&g.boxes[2], rects[2], 0));
        assert_eq!(b.y, a.y);
        assert_eq!(c.y, d.y);
        assert!(b.x > a.x && c.x < d.x);
    }

    fn spread(g: &Graph) -> Vec<Rect> {
        g.boxes
            .iter()
            .enumerate()
            .map(|(i, b)| rect_of(b, Pos2::new(i as f32 * 300.0, i as f32 * 40.0)))
            .collect()
    }

    #[test]
    fn a_port_is_found_by_its_dot_and_only_within_reach() {
        let g = graph();
        let rects = spread(&g);
        let cutoff_in = input_pos(&g.boxes[2], rects[2], 1);
        let near = cutoff_in + Vec2::new(3.0, -2.0);
        assert_eq!(
            input_at(&g, &rects, near, 6.0),
            Some(PortRef { node: 2, port: 1 })
        );
        assert_eq!(input_at(&g, &rects, near, 2.0), None);
        // An input's dot is not an output's.
        assert_eq!(output_at(&g, &rects, cutoff_in, 6.0), None);
        let osc_out = output_pos(&g.boxes[1], rects[1], 0);
        assert_eq!(
            output_at(&g, &rects, osc_out, 6.0),
            Some(PortRef { node: 1, port: 0 })
        );
    }

    #[test]
    fn of_two_ports_in_reach_the_nearer_is_found() {
        let g = graph();
        let rects = spread(&g);
        let [first, second] = [0, 1].map(|i| input_pos(&g.boxes[2], rects[2], i));
        let nearer_second = first + (second - first) * 0.6;
        assert_eq!(
            input_at(&g, &rects, nearer_second, LINE),
            Some(PortRef { node: 2, port: 1 })
        );
    }

    #[test]
    fn a_wire_is_found_along_its_curve_and_not_off_it() {
        let g = graph();
        let rects = spread(&g);
        let index = 0;
        let [a, b, c, d] = wire_curve(&g, &rects, &g.wires[index]);
        // The curve's midpoint, t = 0.5.
        let middle =
            ((a.to_vec2() + b.to_vec2() * 3.0 + c.to_vec2() * 3.0 + d.to_vec2()) / 8.0).to_pos2();
        assert_eq!(wire_at(&g, &rects, middle, 2.0), Some(index));
        assert_eq!(
            wire_at(&g, &rects, middle + Vec2::new(0.0, 200.0), 2.0),
            None
        );
    }

    #[test]
    fn a_point_hits_the_topmost_box_under_it_or_nothing() {
        let rects = [
            Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0)),
            Rect::from_min_size(Pos2::new(50.0, 50.0), Vec2::splat(100.0)),
        ];
        assert_eq!(hit(&rects, Pos2::new(10.0, 10.0)), Some(0));
        assert_eq!(hit(&rects, Pos2::new(75.0, 75.0)), Some(1));
        assert_eq!(hit(&rects, Pos2::new(140.0, 140.0)), Some(1));
        assert_eq!(hit(&rects, Pos2::new(200.0, 10.0)), None);
    }

    #[test]
    fn bounds_hold_every_rect() {
        let rects = [
            Rect::from_min_size(Pos2::new(-10.0, 5.0), Vec2::splat(20.0)),
            Rect::from_min_size(Pos2::new(50.0, 50.0), Vec2::splat(100.0)),
        ];
        assert_eq!(
            bounds(rects),
            Some(Rect::from_min_max(
                Pos2::new(-10.0, 5.0),
                Pos2::new(150.0, 150.0)
            ))
        );
        assert_eq!(bounds([]), None);
    }
}

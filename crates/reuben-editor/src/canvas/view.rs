//! Painting the canvas. Everything decided here is a colour, a stroke or a font; where things are
//! is [`geometry`](super::geometry)'s and [`Camera`]'s.

use std::collections::BTreeMap;

use eframe::egui::epaint::CubicBezierShape;
use eframe::egui::{
    Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Visuals,
};

use super::camera::Camera;
use super::geometry::{
    curve, header_text, input_pos, output_pos, wire_curve, LINE, PAD, PORT_RADIUS, TEXT_SIZE,
};
use super::graph::{Graph, NodeBox, PortKind, PortRef};

/// Below this many points text is a smear, so it is left out and the boxes read as shapes.
const MIN_TEXT: f32 = 5.0;

const WIRE_WIDTH: f32 = 1.5;

/// How much of its colour a wire keeps while a box it does not touch is selected.
const UNSELECTED_WIRE: f32 = 0.2;

/// How much of its colour an input keeps while a wire is drawn that it does not take.
const REFUSING_PORT: f32 = 0.25;

fn kind_color(kind: PortKind) -> Color32 {
    match kind {
        PortKind::Signal => Color32::from_rgb(0xE8, 0x9A, 0x3C),
        PortKind::Value => Color32::from_rgb(0x5B, 0xA8, 0xE6),
        PortKind::Int => Color32::from_rgb(0x4F, 0xC4, 0xB8),
        PortKind::Enum => Color32::from_rgb(0xB0, 0x8C, 0xE8),
        PortKind::Message => Color32::from_rgb(0x6C, 0xC2, 0x6C),
        PortKind::Harmony => Color32::from_rgb(0xE8, 0x7C, 0xB4),
        PortKind::Vocab => Color32::from_rgb(0xD8, 0xC8, 0x50),
        PortKind::Text => Color32::from_rgb(0xC8, 0xC8, 0xB0),
        PortKind::Arg => Color32::from_rgb(0xD0, 0x78, 0x60),
        PortKind::Unknown => Color32::from_rgb(0x99, 0x99, 0x99),
    }
}

/// One frame of the canvas, with everything it needs to know.
pub struct Scene<'a> {
    pub graph: &'a Graph,
    /// Each box's rectangle in canvas units, indexed as `graph.boxes`.
    pub rects: &'a [Rect],
    pub camera: Camera,
    /// The top-left corner of the canvas's area of the window.
    pub origin: Pos2,
    pub selected: Vec<usize>,
    pub wire: Option<usize>,
    pub wiring: Option<Wiring<'a>>,
}

/// A wire being drawn from an output to the pointer.
pub struct Wiring<'a> {
    pub from: PortRef,
    /// The loose end, in canvas units.
    pub to: Pos2,
    /// Per input, `None` when it takes the wire; see `Drag::Wire`.
    pub verdicts: &'a BTreeMap<PortRef, Option<String>>,
}

impl Scene<'_> {
    pub fn paint(&self, painter: &Painter, visuals: &Visuals) {
        let clip = painter.clip_rect();
        painter.rect_filled(clip, CornerRadius::ZERO, visuals.extreme_bg_color);
        self.wires(painter, clip);
        for (i, rect) in self.rects.iter().enumerate() {
            if clip.intersects(self.camera.rect_to_screen(self.origin, *rect)) {
                self.node(painter, visuals, i, *rect, self.selected.contains(&i));
            }
        }
        self.wiring(painter, visuals);
    }

    fn wiring(&self, painter: &Painter, visuals: &Visuals) {
        let Some(wiring) = &self.wiring else {
            return;
        };
        let b = &self.graph.boxes[wiring.from.node];
        let from = output_pos(b, self.rects[wiring.from.node], wiring.from.port);
        let points = curve(from, wiring.to).map(|p| self.at(p));
        let color = kind_color(b.outputs[wiring.from.port].kind);
        let width = (WIRE_WIDTH * self.camera.zoom).max(1.0) * 1.6;
        painter.add(CubicBezierShape::from_points_stroke(
            points,
            false,
            Color32::TRANSPARENT,
            Stroke::new(width, color),
        ));
        let ring = Stroke::new(2.0, visuals.selection.stroke.color);
        for (port, verdict) in wiring.verdicts {
            if verdict.is_none() {
                let b = &self.graph.boxes[port.node];
                let at = input_pos(b, self.rects[port.node], port.port);
                painter.circle_stroke(self.at(at), 2.0 * PORT_RADIUS * self.camera.zoom, ring);
            }
        }
    }

    fn at(&self, p: Pos2) -> Pos2 {
        self.camera.to_screen(self.origin, p)
    }

    fn wires(&self, painter: &Painter, clip: Rect) {
        let width = (WIRE_WIDTH * self.camera.zoom).max(1.0);
        for (index, wire) in self.graph.wires.iter().enumerate() {
            let points = wire_curve(self.graph, self.rects, wire).map(|p| self.at(p));
            // A Bézier stays inside the hull of its control points.
            if !clip.intersects(Rect::from_points(&points)) {
                continue;
            }
            let color = kind_color(wire.kind);
            let touches = |i: &usize| wire.from.node == *i || wire.to.node == *i;
            let stroke = if self.wire == Some(index) {
                Stroke::new(width * 2.4, color)
            } else if self.selected.iter().any(touches) {
                Stroke::new(width * 1.6, color)
            } else if self.selected.is_empty() && self.wire.is_none() {
                Stroke::new(width, color.gamma_multiply(0.8))
            } else {
                Stroke::new(width, color.gamma_multiply(UNSELECTED_WIRE))
            };
            painter.add(CubicBezierShape::from_points_stroke(
                points,
                false,
                Color32::TRANSPARENT,
                stroke,
            ));
        }
    }

    fn node(&self, painter: &Painter, visuals: &Visuals, i: usize, rect: Rect, selected: bool) {
        let b: &NodeBox = &self.graph.boxes[i];
        let zoom = self.camera.zoom;
        let screen = self.camera.rect_to_screen(self.origin, rect);
        let (fill, radius) = if b.is_boundary() {
            (visuals.widgets.inactive.bg_fill, screen.height() / 2.0)
        } else {
            (visuals.window_fill, 4.0 * zoom)
        };
        let outline = if selected {
            Stroke::new(2.0, visuals.selection.stroke.color)
        } else if b.problems.iter().any(|p| p.error) {
            Stroke::new(1.5, visuals.error_fg_color)
        } else if !b.problems.is_empty() {
            Stroke::new(1.5, visuals.warn_fg_color)
        } else {
            visuals.widgets.noninteractive.bg_stroke
        };
        painter.rect(screen, radius, fill, outline, StrokeKind::Outside);

        let font = FontId::monospace(TEXT_SIZE * zoom);
        let legible = font.size >= MIN_TEXT;
        let text = |pos: Pos2, anchor: Align2, text: &str, color: Color32| {
            if legible && !text.is_empty() {
                painter.text(self.at(pos), anchor, text, font.clone(), color);
            }
        };
        if b.is_boundary() {
            let left = Pos2::new(rect.left() + PAD, rect.center().y);
            text(left, Align2::LEFT_CENTER, &b.title, visuals.text_color());
        } else {
            let [title, subtitle] = header_text(rect);
            text(
                title,
                Align2::LEFT_CENTER,
                &b.title,
                visuals.strong_text_color(),
            );
            text(
                subtitle,
                Align2::LEFT_CENTER,
                &b.subtitle,
                visuals.weak_text_color(),
            );
            let rule = subtitle.y + LINE / 2.0 + PAD / 2.0;
            painter.line_segment(
                [
                    self.at(Pos2::new(rect.left(), rule)),
                    self.at(Pos2::new(rect.right(), rule)),
                ],
                visuals.widgets.noninteractive.bg_stroke,
            );
        }

        let dot = |at: Pos2, color: Color32| {
            painter.circle(
                self.at(at),
                PORT_RADIUS * zoom,
                color,
                Stroke::new(1.0, fill),
            );
        };
        for (p, port) in b.inputs.iter().enumerate() {
            let at = input_pos(b, rect, p);
            let mut color = kind_color(port.kind);
            if let Some(wiring) = &self.wiring {
                let takes = wiring.verdicts.get(&PortRef { node: i, port: p });
                if !matches!(takes, Some(None)) {
                    color = color.gamma_multiply(REFUSING_PORT);
                }
            }
            dot(at, color);
            let label = Pos2::new(at.x + PAD, at.y);
            text(label, Align2::LEFT_CENTER, &port.name, visuals.text_color());
        }
        for (p, port) in b.outputs.iter().enumerate() {
            let at = output_pos(b, rect, p);
            dot(at, kind_color(port.kind));
            let label = Pos2::new(at.x - PAD, at.y);
            text(
                label,
                Align2::RIGHT_CENTER,
                &port.name,
                visuals.text_color(),
            );
        }
    }
}

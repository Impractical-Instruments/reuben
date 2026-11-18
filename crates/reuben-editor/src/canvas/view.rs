//! Painting the canvas. Everything decided here is a colour, a stroke or a font; where things are
//! is [`geometry`](super::geometry)'s and [`Camera`]'s.

use eframe::egui::epaint::CubicBezierShape;
use eframe::egui::{
    Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Visuals,
};

use super::camera::Camera;
use super::geometry::{
    header_text, input_pos, output_pos, wire_curve, LINE, PAD, PORT_RADIUS, TEXT_SIZE,
};
use super::graph::{Graph, NodeBox, PortKind};

/// Below this many points text is a smear, so it is left out and the boxes read as shapes.
const MIN_TEXT: f32 = 5.0;

const WIRE_WIDTH: f32 = 1.5;

/// How much of its colour a wire keeps while a box it does not touch is selected.
const UNSELECTED_WIRE: f32 = 0.2;

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
    pub selected: Option<usize>,
}

impl Scene<'_> {
    pub fn paint(&self, painter: &Painter, visuals: &Visuals) {
        let clip = painter.clip_rect();
        painter.rect_filled(clip, CornerRadius::ZERO, visuals.extreme_bg_color);
        self.wires(painter, clip);
        for (i, (b, rect)) in self.graph.boxes.iter().zip(self.rects).enumerate() {
            if clip.intersects(self.camera.rect_to_screen(self.origin, *rect)) {
                self.node(painter, visuals, b, *rect, self.selected == Some(i));
            }
        }
    }

    fn at(&self, p: Pos2) -> Pos2 {
        self.camera.to_screen(self.origin, p)
    }

    fn wires(&self, painter: &Painter, clip: Rect) {
        let width = (WIRE_WIDTH * self.camera.zoom).max(1.0);
        for wire in &self.graph.wires {
            let points = wire_curve(self.graph, self.rects, wire).map(|p| self.at(p));
            // A Bézier stays inside the hull of its control points.
            if !clip.intersects(Rect::from_points(&points)) {
                continue;
            }
            let color = kind_color(wire.kind);
            let stroke = match self.selected {
                None => Stroke::new(width, color.gamma_multiply(0.8)),
                Some(i) if wire.from.node == i || wire.to.node == i => {
                    Stroke::new(width * 1.6, color)
                }
                Some(_) => Stroke::new(width, color.gamma_multiply(UNSELECTED_WIRE)),
            };
            painter.add(CubicBezierShape::from_points_stroke(
                points,
                false,
                Color32::TRANSPARENT,
                stroke,
            ));
        }
    }

    fn node(&self, painter: &Painter, visuals: &Visuals, b: &NodeBox, rect: Rect, selected: bool) {
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

        let dot = |at: Pos2, kind: PortKind| {
            painter.circle(
                self.at(at),
                PORT_RADIUS * zoom,
                kind_color(kind),
                Stroke::new(1.0, fill),
            );
        };
        for (i, port) in b.inputs.iter().enumerate() {
            let at = input_pos(b, rect, i);
            dot(at, port.kind);
            let label = Pos2::new(at.x + PAD, at.y);
            text(label, Align2::LEFT_CENTER, &port.name, visuals.text_color());
        }
        for (i, port) in b.outputs.iter().enumerate() {
            let at = output_pos(b, rect, i);
            dot(at, port.kind);
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
